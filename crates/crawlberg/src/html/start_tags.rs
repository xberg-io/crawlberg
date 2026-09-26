//! How an HTML parser reads a document: which start tags are tags, which bytes are raw text, and
//! which `<base href>` sets the document's base address.
//!
//! tl reads every `<name ...>` as a tag, even inside `<title>` or `<script>`, where a browser
//! reads it as text. html5ever's tokenizer, steered by its tree builder, follows the WHATWG
//! rules for raw text, foreign content (SVG and MathML) and integration points.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeBuilder, TreeBuilderOpts, TreeSink};
use html5ever::{Attribute, LocalName, QualName, TokenizerResult, local_name, ns};
use memchr::{memchr, memchr_iter};

/// The attributes a caller keeps for an element: each attribute name, with data of its own.
pub(super) type Kept<T> = &'static [(&'static str, T)];

/// What an HTML parser reads in a document.
pub(super) struct Scan {
    /// The start tags it reads as tags, each with the attributes the caller keeps.
    pub(super) tags: RealTags,
    /// The byte ranges of raw-text content, in document order.
    pub(super) raw_text: Vec<Range<usize>>,
    /// The decoded `href` of the first `<base>` in the document that has one.
    pub(super) base_href: Option<String>,
}

/// The start tags an HTML parser reads as tags, each with the attributes a caller keeps.
#[cfg_attr(test, derive(Debug, PartialEq))]
pub(super) struct RealTags {
    tags: Vec<RealTag>,
    attrs: Vec<Attribute>,
}

/// A start tag: the offset just past its `>`, its name, and its kept attributes in
/// [`RealTags::attrs`].
#[cfg_attr(test, derive(Debug, PartialEq))]
struct RealTag {
    end: usize,
    name: LocalName,
    attrs: Range<usize>,
}

impl RealTags {
    /// The kept attributes of the start tag named `name`, in any case, that ends at `end`.
    pub(super) fn find(&self, end: usize, name: &[u8]) -> Option<&[Attribute]> {
        let tag = &self.tags[self.tags.binary_search_by_key(&end, |tag| tag.end).ok()?];
        tag.name
            .as_bytes()
            .eq_ignore_ascii_case(name)
            .then(|| &self.attrs[tag.attrs.clone()])
    }
}

/// Read `html` as an HTML parser does, with scripting on or off. Each start tag it reads as a tag
/// is kept with the attributes that `keep(element)` lists first in each of its pairs; a tag with
/// none of them is left out.
///
/// ~keep The tokenizer reports no source offsets, so the input is fed in pieces that each end
/// ~keep at a `>`. A start tag is emitted while the piece holding its closing `>` is fed, so the
/// ~keep offset fed so far is the end of that tag, and the start of any raw text it opens. The
/// ~keep tag's start is not observable, so a caller matches on the end and the name together.
pub(super) fn scan<T: 'static>(html: &str, scripting: bool, keep: fn(&str) -> Option<Kept<T>>) -> Scan {
    let options = TreeBuilderOpts {
        scripting_enabled: scripting,
        ..TreeBuilderOpts::default()
    };
    let sink = Recorder {
        tree: TreeBuilder::new(Names::default(), options),
        html,
        fed: Cell::new(0),
        keep,
        found: RefCell::new(RealTags {
            tags: Vec::new(),
            attrs: Vec::new(),
        }),
        open: Cell::new(None),
        raw_text: RefCell::new(Vec::new()),
    };
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let input = BufferQueue::default();
    for piece in html.split_inclusive('>') {
        tokenizer.sink.fed.set(tokenizer.sink.fed.get() + piece.len());
        input.push_back(StrTendril::from(piece));
        while let TokenizerResult::Script(_) = tokenizer.feed(&input) {}
    }
    tokenizer.end();
    let sink = tokenizer.sink;
    Scan {
        tags: sink.found.into_inner(),
        raw_text: sink.raw_text.into_inner(),
        base_href: sink.tree.sink.base_href.into_inner(),
    }
}

/// Raw-text content the tokenizer is inside: where it starts, and how its end is found.
#[derive(Clone, Copy)]
struct OpenRawText {
    start: usize,
    end: RawTextEnd,
}

/// How the end of raw-text content is found in the source.
#[derive(Clone, Copy)]
enum RawTextEnd {
    /// At the first end tag with the element's name (RCDATA and RAWTEXT), found by
    /// [`end_tag_offset`] once the element is closed.
    EndTag,
    /// At the `<` after the ones the tokenizer emitted as script text, counted so far.
    ///
    /// ~keep Inside `<!--<script>` a script's first `</script>` does not end it, so its end is
    /// ~keep not the first end tag. Script text holds no character references, so every `<`
    /// ~keep in the source up to the closing end tag reaches the sink as text.
    Script { text_lts: usize },
    /// At the end of input: PLAINTEXT content has no end tag.
    EndOfInput,
}

/// Forwards every token to the tree builder, which steers the tokenizer, and records each start
/// tag with a kept attribute and each run of raw-text content.
struct Recorder<'h, T: 'static> {
    tree: TreeBuilder<Rc<Node>, Names>,
    html: &'h str,
    fed: Cell<usize>,
    keep: fn(&str) -> Option<Kept<T>>,
    found: RefCell<RealTags>,
    open: Cell<Option<OpenRawText>>,
    raw_text: RefCell<Vec<Range<usize>>>,
}

impl<T: 'static> Recorder<'_, T> {
    /// Record the start tag `tag` when it carries a kept attribute.
    fn keep_start_tag(&self, tag: &Tag) {
        let Some(keep) = (self.keep)(&tag.name) else {
            return;
        };
        let found = &mut *self.found.borrow_mut();
        let first = found.attrs.len();
        let kept = tag
            .attrs
            .iter()
            .filter(|attr| keep.iter().any(|(name, _)| *name == &*attr.name.local));
        found.attrs.extend(kept.cloned());
        if found.attrs.len() > first {
            found.tags.push(RealTag {
                end: self.fed.get(),
                name: tag.name.clone(),
                attrs: first..found.attrs.len(),
            });
        }
    }

    /// Follow `token` through open raw-text content: count the `<` of script text, and close the
    /// content at any other token but a parse error, which only its end tag or the end of input
    /// can be. Content without an end tag runs to the end of input.
    fn follow_raw_text(&self, open: OpenRawText, token: &Token) {
        let bytes = self.html.as_bytes();
        let end = match (open.end, token) {
            (_, Token::NullCharacterToken | Token::ParseError(_)) => return,
            (RawTextEnd::Script { text_lts }, Token::CharacterTokens(text)) => {
                let text_lts = text_lts + memchr_iter(b'<', text.as_bytes()).count();
                self.open.set(Some(OpenRawText {
                    end: RawTextEnd::Script { text_lts },
                    ..open
                }));
                return;
            }
            (RawTextEnd::EndTag | RawTextEnd::EndOfInput, Token::CharacterTokens(_)) => return,
            (RawTextEnd::Script { text_lts }, _) => memchr_iter(b'<', &bytes[open.start..])
                .nth(text_lts)
                .map_or(bytes.len(), |offset| open.start + offset),
            (RawTextEnd::EndTag, Token::TagToken(tag)) => {
                end_tag_offset(bytes, open.start, tag.name.as_bytes()).unwrap_or(bytes.len())
            }
            (RawTextEnd::EndTag | RawTextEnd::EndOfInput, _) => bytes.len(),
        };
        self.raw_text.borrow_mut().push(open.start..end);
        self.open.set(None);
    }
}

/// Offset of the `</name` that closes RCDATA or RAWTEXT content starting at `from`.
///
/// Per HTML5 the name must be followed by whitespace, `/` or `>`, so `</titles>` does not close a
/// `<title>`.
fn end_tag_offset(bytes: &[u8], from: usize, name: &[u8]) -> Option<usize> {
    let mut index = from;
    while let Some(offset) = bytes.get(index..).and_then(|rest| memchr(b'<', rest)) {
        let at = index + offset;
        let name_start = at + 2;
        let name_end = name_start + name.len();
        if bytes.get(at + 1) == Some(&b'/')
            && bytes
                .get(name_start..name_end)
                .is_some_and(|found| found.eq_ignore_ascii_case(name))
            && bytes
                .get(name_end)
                .is_none_or(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>'))
        {
            return Some(at);
        }
        index = at + 1;
    }
    None
}

impl<T: 'static> TokenSink for Recorder<'_, T> {
    type Handle = Rc<Node>;

    fn process_token(&self, token: Token, line_number: u64) -> TokenSinkResult<Rc<Node>> {
        if let Some(open) = self.open.get() {
            self.follow_raw_text(open, &token);
        }
        let start_tag = match &token {
            Token::TagToken(tag) if tag.kind == TagKind::StartTag => {
                self.keep_start_tag(tag);
                true
            }
            _ => false,
        };
        let result = self.tree.process_token(token, line_number);
        if start_tag {
            let end = match result {
                TokenSinkResult::RawData(RawKind::ScriptData) => Some(RawTextEnd::Script { text_lts: 0 }),
                TokenSinkResult::RawData(_) => Some(RawTextEnd::EndTag),
                TokenSinkResult::Plaintext => Some(RawTextEnd::EndOfInput),
                _ => None,
            };
            if let Some(end) = end {
                self.open.set(Some(OpenRawText {
                    start: self.fed.get(),
                    end,
                }));
            }
        }
        result
    }

    fn end(&self) {
        self.tree.end();
    }

    fn adjusted_current_node_present_but_not_in_html_namespace(&self) -> bool {
        self.tree.adjusted_current_node_present_but_not_in_html_namespace()
    }
}

/// A node of the tree the builder keeps: only what it asks about, the element name and whether
/// a MathML `annotation-xml` is an HTML integration point, and what the base address needs.
struct Node {
    name: QualName,
    annotation_xml_integration_point: bool,
    /// The `href` of an HTML `<base>`.
    base_href: Option<String>,
    /// Whether the node sits inside template contents, which are not part of the document.
    inert: Cell<bool>,
}

impl Node {
    /// A document or comment node, which the builder never asks the name of.
    fn unnamed() -> Rc<Self> {
        Rc::new(Self {
            name: QualName::new(None, ns!(), LocalName::from("")),
            annotation_xml_integration_point: false,
            base_href: None,
            inert: Cell::new(false),
        })
    }
}

/// A tree sink that keeps no tree: the builder needs element names to track the open elements,
/// and the first `<base href>` placed in the document is recorded.
struct Names {
    document: Rc<Node>,
    base_href: RefCell<Option<String>>,
}

impl Default for Names {
    fn default() -> Self {
        Self {
            document: Node::unnamed(),
            base_href: RefCell::new(None),
        }
    }
}

impl Names {
    /// Place `child` under a parent that is `inert` or not.
    ///
    /// ~keep A `<base>` counts once it is placed in the document, not when it is created: the
    /// ~keep builder creates the elements of a `<template>` too, and puts them in its contents.
    fn place(&self, inert: bool, child: &NodeOrText<Rc<Node>>) {
        let NodeOrText::AppendNode(child) = child else {
            return;
        };
        if inert {
            child.inert.set(true);
        } else if let Some(href) = &child.base_href
            && self.base_href.borrow().is_none()
        {
            *self.base_href.borrow_mut() = Some(href.clone());
        }
    }
}

impl TreeSink for Names {
    type Handle = Rc<Node>;
    type Output = ();
    type ElemName<'a> = &'a QualName;

    fn finish(self) {}

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> Rc<Node> {
        Rc::clone(&self.document)
    }

    fn elem_name<'a>(&'a self, target: &'a Rc<Node>) -> &'a QualName {
        &target.name
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> Rc<Node> {
        let base_href = (name.ns == ns!(html) && name.local == local_name!("base"))
            .then(|| attrs.into_iter().find(|attr| attr.name.local == local_name!("href")))
            .flatten()
            .map(|attr| attr.value.to_string());
        Rc::new(Node {
            name,
            annotation_xml_integration_point: flags.mathml_annotation_xml_integration_point,
            base_href,
            inert: Cell::new(false),
        })
    }

    fn create_comment(&self, _text: StrTendril) -> Rc<Node> {
        Node::unnamed()
    }

    fn create_pi(&self, _target: StrTendril, _data: StrTendril) -> Rc<Node> {
        Node::unnamed()
    }

    fn append(&self, parent: &Rc<Node>, child: NodeOrText<Rc<Node>>) {
        self.place(parent.inert.get(), &child);
    }

    fn append_based_on_parent_node(&self, element: &Rc<Node>, prev_element: &Rc<Node>, child: NodeOrText<Rc<Node>>) {
        self.place(element.inert.get() || prev_element.inert.get(), &child);
    }

    fn append_doctype_to_document(&self, _name: StrTendril, _public_id: StrTendril, _system_id: StrTendril) {}

    fn get_template_contents(&self, _target: &Rc<Node>) -> Rc<Node> {
        let contents = Node::unnamed();
        contents.inert.set(true);
        contents
    }

    fn same_node(&self, x: &Rc<Node>, y: &Rc<Node>) -> bool {
        Rc::ptr_eq(x, y)
    }

    fn set_quirks_mode(&self, _mode: QuirksMode) {}

    fn append_before_sibling(&self, _sibling: &Rc<Node>, _new_node: NodeOrText<Rc<Node>>) {}

    fn add_attrs_if_missing(&self, _target: &Rc<Node>, _attrs: Vec<Attribute>) {}

    fn remove_from_parent(&self, _target: &Rc<Node>) {}

    fn reparent_children(&self, _node: &Rc<Node>, _new_parent: &Rc<Node>) {}

    fn is_mathml_annotation_xml_integration_point(&self, handle: &Rc<Node>) -> bool {
        handle.annotation_xml_integration_point
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_href(html: &str) -> Option<String> {
        scan(html, false, |_| None::<Kept<()>>).base_href
    }

    #[test]
    fn should_take_the_base_href_only_from_an_html_base_in_the_document() {
        assert_eq!(
            base_href(r#"<svg><base href="/svg/"></svg><base target="x"><base href="/html/">"#).as_deref(),
            Some("/html/"),
            "an SVG `<base>` and a `<base>` without `href` do not count"
        );
        assert_eq!(
            base_href(r#"<template><div><base href="/tpl/"></div></template><base href="/doc/">"#).as_deref(),
            Some("/doc/"),
            "a `<base>` nested in template contents does not count"
        );
    }

    #[test]
    fn should_take_the_base_href_from_a_foster_parented_base() {
        assert_eq!(
            base_href(r#"<table><base href="/table/"></table>"#).as_deref(),
            Some("/table/"),
            "a browser moves a `<base>` in a table in front of the table, into the document"
        );
        assert_eq!(
            base_href(r#"<template><table><base href="/tpl/"></table></template>"#),
            None,
            "moved in front of a table inside template contents, it stays out of the document"
        );
    }
}
