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
use memchr::{memchr, memchr_iter, memchr2_iter};

/// What an HTML parser reads in a document.
pub(super) struct Scan<'h> {
    /// The document as it was read: the source, or the source with the attributes past
    /// [`ATTRIBUTE_LIMIT`] of an over-wide tag overwritten. It has the source's byte length, and
    /// the spans and ranges below address it.
    pub(super) text: Cow<'h, str>,
    /// The start tags it reads as tags, of the elements the caller keeps.
    pub(super) tags: RealTags,
    /// The byte ranges of raw-text content, in document order.
    pub(super) raw_text: Vec<Range<usize>>,
    /// The decoded `href` of the first `<base>` in the document that has one.
    pub(super) base_href: Option<String>,
}

/// The start tags an HTML parser reads as tags, in document order, each with every attribute it
/// reads on the tag.
#[cfg_attr(test, derive(Debug, PartialEq))]
pub(super) struct RealTags {
    tags: Vec<RealTag>,
    attrs: Vec<Attribute>,
}

/// A start tag: its span in the source, its name, whether it is self-closing, and its
/// attributes in [`RealTags::attrs`].
#[cfg_attr(test, derive(Debug, PartialEq))]
struct RealTag {
    span: Range<usize>,
    name: LocalName,
    self_closing: bool,
    attrs: Range<usize>,
}

/// A start tag as an HTML parser reads it.
pub(super) struct StartTag<'t> {
    /// The tag's bytes in the source, from its `<` to just past its `>`.
    pub(super) span: Range<usize>,
    /// The element name, lower-cased.
    pub(super) name: &'t str,
    /// Whether the tag ends in `/>`.
    pub(super) self_closing: bool,
    /// The attributes in source order: the first copy of each name, value decoded.
    pub(super) attrs: &'t [Attribute],
}

impl RealTags {
    /// Every start tag, in document order. The spans do not overlap.
    pub(super) fn iter(&self) -> impl Iterator<Item = StartTag<'_>> {
        self.tags.iter().map(|tag| StartTag {
            span: tag.span.clone(),
            name: &tag.name,
            self_closing: tag.self_closing,
            attrs: &self.attrs[tag.attrs.clone()],
        })
    }
}

/// Read `html` as an HTML parser does, with scripting on or off. Each start tag it reads as a tag
/// is kept, with its span and attributes, when `keep(element)` holds.
///
/// ~keep The tokenizer reports no source offsets, so the input is fed in pieces: each `<` alone,
/// ~keep and the bytes between, cut after every `>`. A start tag is emitted while the piece
/// ~keep holding its closing `>` is fed, so the offset fed so far is the end of that tag, and the
/// ~keep start of any raw text it opens. Its start is found from the tokens before it: see
/// ~keep [`Recorder::next_start`].
///
/// Before each piece is fed, [`AttributeBound`] overwrites the attributes past the limit of an
/// over-wide tag, and it is that text which is read and returned: a caller writes its output
/// from [`Scan::text`], so the markdown converter and `tl` read the same bytes html5ever read.
pub(super) fn scan(source: &str, scripting: bool, keep: fn(&str) -> bool) -> Scan<'_> {
    let options = TreeBuilderOpts {
        scripting_enabled: scripting,
        ..TreeBuilderOpts::default()
    };
    let sink = Recorder {
        tree: TreeBuilder::new(Names::default(), options),
        text: RefCell::new(Cow::Borrowed(source)),
        piece: Cell::new(0..0),
        next_start: Cell::new(0),
        keep,
        found: RefCell::new(RealTags {
            tags: Vec::new(),
            attrs: Vec::new(),
        }),
        open: Cell::new(None),
        raw_text: RefCell::new(Vec::new()),
        tokens: Cell::new(0),
    };
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let input = BufferQueue::default();
    let mut bound = AttributeBound::default();
    let mut feed = |piece: Range<usize>| {
        let mut from = piece.start;
        while from < piece.end {
            let tokens = tokenizer.sink.tokens.get();
            let mut text = tokenizer.sink.text.borrow_mut();
            let until = bound.follow(source, from..piece.end, tokens, &mut text);
            input.push_back(StrTendril::from(&text[from..until]));
            drop(text);
            tokenizer.sink.piece.set(from..until);
            while let TokenizerResult::Script(_) = tokenizer.feed(&input) {}
            from = until;
        }
    };
    let mut from = 0;
    for at in memchr2_iter(b'<', b'>', source.as_bytes()) {
        if source.as_bytes()[at] == b'<' {
            feed(from..at);
            feed(at..at + 1);
        } else {
            feed(from..at + 1);
        }
        from = at + 1;
    }
    feed(from..source.len());
    tokenizer.end();
    let sink = tokenizer.sink;
    Scan {
        text: sink.text.into_inner(),
        tags: sink.found.into_inner(),
        raw_text: sink.raw_text.into_inner(),
        base_href: sink.tree.sink.base_href.into_inner(),
    }
}

/// The most attributes [`AttributeBound`] lets one tag carry into the tokenizer.
///
/// ~keep html5ever checks each new attribute name against every earlier one on the tag
/// ~keep (`finish_attribute`, 0.40.1), so a tag with n names costs n²/2 comparisons, and a page
/// ~keep author picks n. Real tags carry tens of attributes. At 1024, each attribute is compared
/// ~keep with at most 1023 others, so the check grows linearly with the page.
const ATTRIBUTE_LIMIT: usize = 1024;

/// A state of a tag being read, from its `<` to its `>`, as html5ever's tokenizer names them.
#[derive(Clone, Copy)]
enum TagState {
    Open,
    EndOpen,
    Name,
    BeforeAttributeName,
    AttributeName,
    AfterAttributeName,
    BeforeAttributeValue,
    DoubleQuoted,
    SingleQuoted,
    Unquoted,
    AfterQuotedValue,
    SelfClosing,
}

/// What one byte does to a tag in `state`: the next state and whether the byte starts an
/// attribute, or `None` when the tag ends at it, or it is not a tag.
fn tag_step(state: TagState, byte: u8) -> Option<(TagState, bool)> {
    use TagState::*;
    let space = matches!(byte, b'\t' | b'\n' | b'\x0C' | b'\r' | b' ');
    let next = match state {
        Open if byte.is_ascii_alphabetic() => (Name, false),
        Open if byte == b'/' => (EndOpen, false),
        EndOpen if byte.is_ascii_alphabetic() => (Name, false),
        Open | EndOpen => return None,
        _ if byte == b'>' && !matches!(state, DoubleQuoted | SingleQuoted) => return None,
        Name | AttributeName | AfterAttributeName | BeforeAttributeName | AfterQuotedValue | SelfClosing
            if byte == b'/' =>
        {
            (SelfClosing, false)
        }
        Name if space => (BeforeAttributeName, false),
        Name => (Name, false),
        AttributeName | AfterAttributeName if space => (AfterAttributeName, false),
        AttributeName | AfterAttributeName if byte == b'=' => (BeforeAttributeValue, false),
        AttributeName => (AttributeName, false),
        BeforeAttributeName | AfterQuotedValue | SelfClosing if space => (BeforeAttributeName, false),
        AfterAttributeName | BeforeAttributeName | AfterQuotedValue | SelfClosing => (AttributeName, true),
        BeforeAttributeValue if space => (BeforeAttributeValue, false),
        BeforeAttributeValue if byte == b'"' => (DoubleQuoted, false),
        BeforeAttributeValue if byte == b'\'' => (SingleQuoted, false),
        BeforeAttributeValue => (Unquoted, false),
        DoubleQuoted if byte == b'"' => (AfterQuotedValue, false),
        SingleQuoted if byte == b'\'' => (AfterQuotedValue, false),
        DoubleQuoted | SingleQuoted => (state, false),
        Unquoted if space => (BeforeAttributeName, false),
        Unquoted => (Unquoted, false),
    };
    Some(next)
}

/// The tag html5ever may be reading: its state, the attributes it has started, and how many
/// tokens html5ever had emitted when it consumed the tag's `<`.
#[derive(Clone, Copy)]
struct TagRun {
    state: TagState,
    attributes: usize,
    tokens: usize,
}

/// Overwrites with spaces the attributes of one tag past the [`ATTRIBUTE_LIMIT`]th, up to the
/// `>` that ends the tag.
///
/// ~keep html5ever does not report its state, so the tag is followed through its attribute
/// ~keep states from the only `<` that can open one: the first `<` html5ever consumes after a
/// ~keep token (or after an empty end tag `</>`, which emits none). In the data state every
/// ~keep other `<` emits a token, and inside a comment, a doctype or a tag the `<` that opened it
/// ~keep came first. From a tag's `<` to its `>` html5ever emits only parse errors, so a token
/// ~keep emitted since shows that the `<` opened no tag, as in raw text, and the run is dropped.
/// ~keep The overwrite starts only once html5ever has read every byte before it without a
/// ~keep token, and it ends at the `>` where the run ends, which is the `>` where html5ever ends
/// ~keep the tag, past any `>` inside a quoted value. Spaces keep the byte length.
#[derive(Default)]
struct AttributeBound {
    run: Option<TagRun>,
    /// Whether the run is past the limit, so its bytes up to its `>` are overwritten.
    overwriting: bool,
    /// Whether the last piece was a `<` that may open a run once html5ever has consumed it.
    after_lt: bool,
    /// How many tokens html5ever had emitted after it consumed the last `<`, `None` after `</>`.
    tokens_at_lt: Option<usize>,
}

impl AttributeBound {
    /// Follow `range` of `source` before it is fed, when html5ever has emitted `tokens` tokens,
    /// and overwrite in `text` what is past the limit. Returns where the part to feed now ends:
    /// the end of `range`, or the start of an overwrite, which waits until html5ever has read
    /// the bytes before it. A `<` is always a range of its own.
    fn follow(&mut self, source: &str, range: Range<usize>, tokens: usize, text: &mut Cow<'_, str>) -> usize {
        if self.run.is_some_and(|run| run.tokens != tokens) {
            debug_assert!(!self.overwriting, "html5ever emitted a token inside an overwritten tag");
            self.run = None;
            self.overwriting = false;
        }
        if std::mem::take(&mut self.after_lt) {
            if self.run.is_none() && self.tokens_at_lt != Some(tokens) {
                self.run = Some(TagRun {
                    state: TagState::Open,
                    attributes: 0,
                    tokens,
                });
            }
            self.tokens_at_lt = Some(tokens);
        }
        let bytes = source.as_bytes();
        let mut overwritten: Option<Range<usize>> = None;
        let mut at = range.start;
        while let Some(mut run) = self.run
            && at < range.end
        {
            if !self.overwriting {
                at += unchanged_prefix(run.state, &bytes[at..range.end]);
                if at == range.end {
                    break;
                }
            }
            match tag_step(run.state, bytes[at]) {
                Some((state, starts_attribute)) => {
                    if starts_attribute && !self.overwriting && run.attributes == ATTRIBUTE_LIMIT {
                        if at > range.start {
                            return at;
                        }
                        self.overwriting = true;
                    }
                    run.state = state;
                    run.attributes += usize::from(starts_attribute);
                    self.run = Some(run);
                }
                None => {
                    if matches!(run.state, TagState::EndOpen) && bytes[at] == b'>' {
                        self.tokens_at_lt = None;
                    }
                    self.run = None;
                    self.overwriting = false;
                    at += 1;
                    continue;
                }
            }
            if self.overwriting {
                overwritten.get_or_insert(at..at).end = at + 1;
            }
            at += 1;
        }
        // ~keep An overwrite starts where an attribute starts, after an ASCII space, `/`, `=` or
        // ~keep quote, or at a range's start, and ends at a `>` or a range's end, so it is on
        // ~keep character boundaries.
        if let Some(overwritten) = overwritten {
            text.to_mut()
                .replace_range(overwritten.clone(), &" ".repeat(overwritten.len()));
        }
        self.after_lt = &bytes[range.clone()] == b"<";
        range.end
    }
}

/// How many leading bytes of `rest` keep a tag in `state` with no attribute started.
fn unchanged_prefix(state: TagState, rest: &[u8]) -> usize {
    let stops: &[u8] = match state {
        TagState::DoubleQuoted => return memchr(b'"', rest).unwrap_or(rest.len()),
        TagState::SingleQuoted => return memchr(b'\'', rest).unwrap_or(rest.len()),
        TagState::Name => b"\t\n\x0C\r />",
        TagState::AttributeName => b"\t\n\x0C\r />=",
        TagState::Unquoted => b"\t\n\x0C\r >",
        _ => return 0,
    };
    rest.iter().position(|byte| stops.contains(byte)).unwrap_or(rest.len())
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
struct Recorder<'h> {
    tree: TreeBuilder<Rc<Node>, Names>,
    /// The document as it is fed: the source, with what [`AttributeBound`] overwrote.
    text: RefCell<Cow<'h, str>>,
    /// The piece of the source being fed.
    piece: Cell<Range<usize>>,
    /// No start tag begins before this offset.
    ///
    /// ~keep After any token but a parse error, the next start tag begins at or after the end of
    /// ~keep the piece being fed, or at the piece itself when it is a lone `<`: a token emitted
    /// ~keep while a lone `<` is fed was held back by what came before it, such as a character
    /// ~keep reference. Between that token and a start tag the tokenizer emits nothing, and every
    /// ~keep `<` it reads there in the data state would emit a token unless it opens that tag or
    /// ~keep is an empty end tag `</>`, which emits nothing. So the tag starts at the first `<`
    /// ~keep from here, and its span takes in any `</>` just before it.
    next_start: Cell<usize>,
    keep: fn(&str) -> bool,
    found: RefCell<RealTags>,
    open: Cell<Option<OpenRawText>>,
    raw_text: RefCell<Vec<Range<usize>>>,
    /// How many tokens but parse errors the tokenizer has emitted.
    tokens: Cell<usize>,
}

impl Recorder<'_> {
    /// The offset fed so far: the end of the piece being fed.
    fn fed(&self) -> usize {
        let piece = self.piece.take();
        let fed = piece.end;
        self.piece.set(piece);
        fed
    }

    /// Record the start tag `tag`, which ends at the offset fed so far, when its element is kept.
    fn keep_start_tag(&self, tag: &Tag) {
        if !(self.keep)(&tag.name) {
            return;
        }
        let end = self.fed();
        let from = self.next_start.get();
        let Some(start) = memchr(b'<', &self.text.borrow().as_bytes()[from..end]).map(|offset| from + offset) else {
            debug_assert!(false, "no `<` before the start tag ending at {end}");
            return;
        };
        let found = &mut *self.found.borrow_mut();
        let first = found.attrs.len();
        found.attrs.extend(tag.attrs.iter().cloned());
        found.tags.push(RealTag {
            span: start..end,
            name: tag.name.clone(),
            self_closing: tag.self_closing,
            attrs: first..found.attrs.len(),
        });
    }

    /// Move [`Self::next_start`] past a token that was just emitted.
    fn after_token(&self) {
        self.tokens.set(self.tokens.get() + 1);
        let piece = self.piece.take();
        let lone_lt = &self.text.borrow().as_bytes()[piece.clone()] == b"<";
        self.next_start.set(if lone_lt { piece.start } else { piece.end });
        self.piece.set(piece);
    }

    /// Follow `token` through open raw-text content: count the `<` of script text, and close the
    /// content at any other token but a parse error, which only its end tag or the end of input
    /// can be. Content without an end tag runs to the end of input.
    fn follow_raw_text(&self, open: OpenRawText, token: &Token) {
        let fed = self.text.borrow();
        let bytes = fed.as_bytes();
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

impl TokenSink for Recorder<'_> {
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
        if !matches!(token, Token::ParseError(_)) {
            self.after_token();
        }
        let result = self.tree.process_token(token, line_number);
        if start_tag {
            let end = match result {
                TokenSinkResult::RawData(RawKind::ScriptData) => Some(RawTextEnd::Script { text_lts: 0 }),
                TokenSinkResult::RawData(_) => Some(RawTextEnd::EndTag),
                TokenSinkResult::Plaintext => Some(RawTextEnd::EndOfInput),
                _ => None,
            };
            if let Some(end) = end {
                self.open.set(Some(OpenRawText { start: self.fed(), end }));
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
    use proptest::prelude::*;

    use super::*;

    fn base_href(html: &str) -> Option<String> {
        scan(html, false, |_| false).base_href
    }

    /// The start and end of each `<a>` start tag in `html`.
    fn link_spans(html: &str) -> Vec<(usize, usize)> {
        scan(html, true, |name| name == "a")
            .tags
            .iter()
            .map(|tag| (tag.span.start, tag.span.end))
            .collect()
    }

    /// A start tag as html5ever reads it: name, attributes and whether it is self-closing.
    type Read = (String, Vec<(String, String)>, bool);

    /// Each start tag html5ever's tokenizer reads in `fragment` on its own, with the offset just
    /// past it.
    fn read_alone(fragment: &str) -> Vec<(usize, Read)> {
        struct Tags {
            fed: Cell<usize>,
            tags: RefCell<Vec<(usize, Read)>>,
        }
        impl TokenSink for Tags {
            type Handle = ();
            fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
                if let Token::TagToken(tag) = token
                    && tag.kind == TagKind::StartTag
                {
                    let attrs = tag
                        .attrs
                        .iter()
                        .map(|attr| (attr.name.local.to_string(), attr.value.to_string()))
                        .collect();
                    let read = (tag.name.to_string(), attrs, tag.self_closing);
                    self.tags.borrow_mut().push((self.fed.get(), read));
                }
                TokenSinkResult::Continue
            }
        }
        let tokenizer = Tokenizer::new(
            Tags {
                fed: Cell::new(0),
                tags: RefCell::new(Vec::new()),
            },
            TokenizerOpts::default(),
        );
        let input = BufferQueue::default();
        for piece in fragment.split_inclusive('>') {
            tokenizer.sink.fed.set(tokenizer.sink.fed.get() + piece.len());
            input.push_back(StrTendril::from(piece));
            let _ = tokenizer.feed(&input);
        }
        tokenizer.end();
        tokenizer.sink.tags.into_inner()
    }

    /// The start tags whose span, read on its own, is not exactly the tag the scan reports.
    fn misread_spans(html: &str) -> Vec<(Range<usize>, String)> {
        scan(html, true, |_| true)
            .tags
            .iter()
            .filter(|tag| {
                let attrs = tag
                    .attrs
                    .iter()
                    .map(|attr| (attr.name.local.to_string(), attr.value.to_string()))
                    .collect();
                let read = (tag.name.to_owned(), attrs, tag.self_closing);
                read_alone(&html[tag.span.clone()]) != [(tag.span.len(), read)]
            })
            .map(|tag| (tag.span.clone(), html[tag.span].to_owned()))
            .collect()
    }

    #[test]
    fn should_find_where_each_start_tag_begins() {
        let wrong = [
            ("\u{feff}<a href=1>", vec![(3, 13)]),
            ("x&amp<a href=1>", vec![(5, 15)]),
            ("&amp<<a href=1>", vec![(5, 15)]),
            ("<!--<a --><a href=1>", vec![(10, 20)]),
            ("a <3 <a href=1>", vec![(5, 15)]),
            ("<title><a </title><a href=1>", vec![(18, 28)]),
            ("<svg><![CDATA[<a ]]><a b=c></svg>", vec![(20, 27)]),
            (r#"<a title="<a href=x>">"#, vec![(0, 22)]),
            (r#"<a href=b ="x>z</a><a href="y">"#, vec![(0, 14), (19, 31)]),
        ]
        .into_iter()
        .filter(|(html, spans)| link_spans(html) != *spans)
        .collect::<Vec<_>>();
        assert!(wrong.is_empty(), "not read as expected: {wrong:?}");
    }

    #[test]
    fn should_read_each_tag_of_the_review_corpus_where_it_stands() {
        // ~keep The corpus from the review of #123, to four bytes: `<a href=1 {s}>` with `s`
        // ~keep over the bytes that move a tag's end, after a character reference, a comment
        // ~keep and CDATA that each hold a `<a`.
        let alphabet = ['a', '=', '"', '\'', ' ', '/', '>', '\t'];
        let mut suffixes = vec![String::new()];
        for _ in 0..4 {
            let longer: Vec<String> = suffixes
                .iter()
                .filter(|s| s.len() == suffixes.last().map_or(0, String::len))
                .flat_map(|s| alphabet.iter().map(move |c| format!("{s}{c}")))
                .collect();
            suffixes.extend(longer);
        }
        assert_eq!(suffixes.len(), 4681, "every suffix up to four bytes");
        let misread: Vec<_> = suffixes
            .iter()
            .flat_map(|s| {
                misread_spans(&format!(
                    "x&amp<a href=1 {s}>z</a><!--<a -->&#<a href=2>w</a><svg><![CDATA[<a ]]><a b=c></svg>"
                ))
            })
            .collect();
        assert!(
            misread.is_empty(),
            "{} tags misread, first: {:?}",
            misread.len(),
            misread.first()
        );
    }

    proptest! {
        /// Each start tag the scan reports is, read on its own, one whole start tag with the
        /// same name and attributes.
        #[test]
        fn each_start_tag_reads_alone_as_the_same_tag(
            html in r#"(<a href=x>|<a b=c ="d>|<img src="y" =">|<a title="<a x>">|<title>|</title>|<script>|</script>|<svg>|</svg>|<!\[CDATA\[|\]\]>|<!--|-->|&amp|&#|&|<3|<<|[a-z0-9 <>"'/=\t-]){0,40}"#
        ) {
            let misread = misread_spans(&html);
            prop_assert!(misread.is_empty(), "misread: {:?}", misread);
        }
    }

    /// ` a0 a1 ...` with `count` distinct attribute names, each followed by `value`.
    fn attributes(count: usize, value: &str) -> String {
        (0..count).map(|i| format!(" a{i}{value}")).collect()
    }

    #[test]
    fn should_read_a_tag_at_the_attribute_limit_unchanged() {
        let html = format!("<div{}>x</div>", attributes(ATTRIBUTE_LIMIT, ""));
        let read = scan(&html, true, |name| name == "div");
        assert!(
            matches!(read.text, Cow::Borrowed(_)),
            "a tag at the limit is read as written"
        );
        let tag = read.tags.iter().next().expect("the div is read");
        assert_eq!(tag.attrs.len(), ATTRIBUTE_LIMIT);
    }

    #[test]
    fn should_read_a_wider_tag_with_the_attributes_past_the_limit_overwritten() {
        let html = format!("<p>a</p><div{}>x<a href=y>", attributes(20 * ATTRIBUTE_LIMIT, ""));
        let read = scan(&html, true, |name| name == "div" || name == "a");
        assert_eq!(read.text.len(), html.len(), "the byte length is kept");
        let kept = format!("<p>a</p><div{} ", attributes(ATTRIBUTE_LIMIT, ""));
        assert!(read.text.starts_with(&kept), "the first attributes are kept");
        assert!(read.text[kept.len()..].trim_start().starts_with(">x<a href=y>"));
        let tags: Vec<_> = read
            .tags
            .iter()
            .map(|tag| (tag.name.to_owned(), tag.attrs.len()))
            .collect();
        assert_eq!(tags, [("div".to_owned(), ATTRIBUTE_LIMIT), ("a".to_owned(), 1)]);
    }

    #[test]
    fn should_bound_a_wide_tag_whatever_its_values_hold() {
        let packed = |value: &str| {
            (0..3 * ATTRIBUTE_LIMIT)
                .map(|i| format!("a{i}{value}"))
                .collect::<String>()
        };
        for (value, list) in [r#"=">""#, r#"="<b >""#, "='<'", r#"="""#, "=x", "/", "=<b"]
            .map(|value| (value, attributes(3 * ATTRIBUTE_LIMIT, value)))
            .into_iter()
            .chain([
                ("packed quoted", format!(" {}", packed(r#"="""#))),
                ("packed slash", format!(" {}", packed("/"))),
            ])
        {
            let div = format!("<div{list}>");
            let html = format!("{div}<a href=y>");
            let read = scan(&html, true, |name| name == "div" || name == "a");
            let tags: Vec<_> = read
                .tags
                .iter()
                .map(|tag| (tag.name.to_owned(), tag.span.clone(), tag.attrs.len()))
                .collect();
            assert!(
                tags.first().is_some_and(|(name, span, count)| name == "div"
                    && *span == (0..div.len())
                    && *count <= ATTRIBUTE_LIMIT),
                "{value}: the div ends where it ends in the source, {:?}",
                tags.first().map(|(name, span, count)| (name, span, count))
            );
            assert_eq!(
                tags.get(1),
                Some(&("a".to_owned(), div.len()..html.len(), 1)),
                "{value}: the next tag is read"
            );
            assert_eq!(tags.len(), 2, "{value}: nothing in the div's values is read as a tag");
        }
    }

    #[test]
    fn should_leave_a_wide_candidate_that_opens_no_tag_unchanged() {
        let wide = attributes(3 * ATTRIBUTE_LIMIT, "");
        for html in [
            format!("<p>a</p><!-- <a{wide} --><p>after"),
            format!(r#"<div title="<b{wide}"><p>after"#),
            format!("<!DOCTYPE html <a{wide}><p>after"),
            format!("<?x <a{wide}><p>after"),
            format!("<textarea><b{wide}></textarea><p>after"),
        ] {
            let read = scan(&html, true, |name| name == "p");
            assert!(
                matches!(read.text, Cow::Borrowed(_)),
                "no tag is read there, so nothing is overwritten: {}",
                &html[..40]
            );
            assert!(
                read.tags.iter().any(|tag| tag.span.end == html.len() - "after".len()),
                "the last <p> is read"
            );
        }
    }

    #[test]
    fn should_bound_a_wide_tag_after_a_less_than_sign_that_opened_none() {
        let wide = attributes(3 * ATTRIBUTE_LIMIT, "");
        for prefix in ["<", "</>", "&amp", "x <3 ", "<!-- x -->", "<title>t</title>"] {
            let html = format!("{prefix}<div{wide}><a href=y>");
            let read = scan(&html, true, |name| name == "div" || name == "a");
            let tags: Vec<_> = read
                .tags
                .iter()
                .map(|tag| (tag.name.to_owned(), tag.attrs.len()))
                .collect();
            assert_eq!(
                tags,
                [("div".to_owned(), ATTRIBUTE_LIMIT), ("a".to_owned(), 1)],
                "after {prefix:?}"
            );
        }
    }

    #[test]
    fn should_bound_a_wide_tag_after_an_open_quote_in_a_comment() {
        // ~keep Read from the `<b`, the quote never closes, so only the run from the `<div`
        // ~keep counts the div's attributes.
        let html = format!(
            r#"<!-- <b x=" --><div{}><a href=y>"#,
            attributes(3 * ATTRIBUTE_LIMIT, "")
        );
        let read = scan(&html, true, |name| name == "div" || name == "a");
        let tags: Vec<_> = read
            .tags
            .iter()
            .map(|tag| (tag.name.to_owned(), tag.attrs.len()))
            .collect();
        assert_eq!(tags, [("div".to_owned(), ATTRIBUTE_LIMIT), ("a".to_owned(), 1)]);
    }

    #[test]
    fn should_leave_script_text_that_reads_like_a_wide_tag_unchanged() {
        let html = format!(
            "<script>if(a<b c=\"0\"{}){{}}</script><a href=y>",
            ";d=\"x\"".repeat(3 * ATTRIBUTE_LIMIT)
        );
        let read = scan(&html, true, |name| name == "a");
        assert!(matches!(read.text, Cow::Borrowed(_)), "script text is never a tag");
        assert_eq!(read.tags.iter().count(), 1, "the link after the script is read");
    }

    #[test]
    fn should_bound_the_attributes_of_an_end_tag() {
        let html = format!("<title>t</title{}><a href=y>", attributes(3 * ATTRIBUTE_LIMIT, ""));
        let read = scan(&html, true, |name| name == "a");
        assert!(matches!(read.text, Cow::Owned(_)), "the end tag is bounded");
        assert_eq!(read.raw_text.len(), 1, "the title's text still ends at its end tag");
        assert_eq!(read.tags.iter().count(), 1, "the link after it is read");
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
