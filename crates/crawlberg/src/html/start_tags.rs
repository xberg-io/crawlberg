//! Which start tags in a document an HTML parser reads as tags.
//!
//! tl reads every `<name ...>` as a tag, even inside `<title>` or `<script>`, where a browser
//! reads it as text. html5ever's tokenizer, steered by its tree builder, follows the WHATWG
//! rules for raw text, foreign content (SVG and MathML) and integration points.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeBuilder, TreeBuilderOpts, TreeSink};
use html5ever::{Attribute, LocalName, QualName, TokenizerResult};

/// The attributes a caller keeps for an element: each attribute name, with data of its own.
pub(super) type Kept<T> = &'static [(&'static str, T)];

/// The start tags an HTML parser reads as tags, each with the attributes a caller keeps.
pub(super) struct RealTags {
    tags: Vec<RealTag>,
    attrs: Vec<Attribute>,
}

/// A start tag: the offset just past its `>`, its name, and its kept attributes in
/// [`RealTags::attrs`].
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

/// Each start tag in `html` that an HTML parser reads as a tag, with the attributes that
/// `keep(element)` lists first in each of its pairs. A tag with none of them is left out.
///
/// ~keep The tokenizer reports no source offsets, so the input is fed in pieces that each end
/// ~keep at a `>`. A start tag is emitted while the piece holding its closing `>` is fed, so the
/// ~keep offset fed so far is the end of that tag. Its start is not observable, so a caller
/// ~keep matches on the end and the name together.
pub(super) fn real_start_tags<T: 'static>(html: &str, keep: fn(&str) -> Option<Kept<T>>) -> RealTags {
    let sink = Recorder {
        tree: TreeBuilder::new(Names::default(), TreeBuilderOpts::default()),
        fed: Cell::new(0),
        keep,
        found: RefCell::new(RealTags {
            tags: Vec::new(),
            attrs: Vec::new(),
        }),
    };
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let input = BufferQueue::default();
    for piece in html.split_inclusive('>') {
        tokenizer.sink.fed.set(tokenizer.sink.fed.get() + piece.len());
        input.push_back(StrTendril::from(piece));
        while let TokenizerResult::Script(_) = tokenizer.feed(&input) {}
    }
    tokenizer.end();
    tokenizer.sink.found.into_inner()
}

/// Forwards every token to the tree builder, which steers the tokenizer, and records each start
/// tag with a kept attribute.
struct Recorder<T: 'static> {
    tree: TreeBuilder<Rc<Node>, Names>,
    fed: Cell<usize>,
    keep: fn(&str) -> Option<Kept<T>>,
    found: RefCell<RealTags>,
}

impl<T: 'static> TokenSink for Recorder<T> {
    type Handle = Rc<Node>;

    fn process_token(&self, token: Token, line_number: u64) -> TokenSinkResult<Rc<Node>> {
        if let Token::TagToken(tag) = &token
            && tag.kind == TagKind::StartTag
            && let Some(keep) = (self.keep)(&tag.name)
        {
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
        self.tree.process_token(token, line_number)
    }

    fn end(&self) {
        self.tree.end();
    }

    fn adjusted_current_node_present_but_not_in_html_namespace(&self) -> bool {
        self.tree.adjusted_current_node_present_but_not_in_html_namespace()
    }
}

/// A node of the tree the builder keeps: only what it asks about, the element name and whether
/// a MathML `annotation-xml` is an HTML integration point.
struct Node {
    name: QualName,
    annotation_xml_integration_point: bool,
}

impl Node {
    /// A document, comment or template contents node, which the builder never asks the name of.
    fn unnamed() -> Rc<Self> {
        Rc::new(Self {
            name: QualName::new(None, html5ever::ns!(), LocalName::from("")),
            annotation_xml_integration_point: false,
        })
    }
}

/// A tree sink that keeps no tree: the builder needs element names to track the open elements,
/// and nothing else here is read.
struct Names {
    document: Rc<Node>,
}

impl Default for Names {
    fn default() -> Self {
        Self {
            document: Node::unnamed(),
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

    fn create_element(&self, name: QualName, _attrs: Vec<Attribute>, flags: ElementFlags) -> Rc<Node> {
        Rc::new(Node {
            name,
            annotation_xml_integration_point: flags.mathml_annotation_xml_integration_point,
        })
    }

    fn create_comment(&self, _text: StrTendril) -> Rc<Node> {
        Node::unnamed()
    }

    fn create_pi(&self, _target: StrTendril, _data: StrTendril) -> Rc<Node> {
        Node::unnamed()
    }

    fn append(&self, _parent: &Rc<Node>, _child: NodeOrText<Rc<Node>>) {}

    fn append_based_on_parent_node(&self, _element: &Rc<Node>, _prev: &Rc<Node>, _child: NodeOrText<Rc<Node>>) {}

    fn append_doctype_to_document(&self, _name: StrTendril, _public_id: StrTendril, _system_id: StrTendril) {}

    fn get_template_contents(&self, _target: &Rc<Node>) -> Rc<Node> {
        Node::unnamed()
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
