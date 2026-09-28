//! Which `<name ...>` runs in a document an HTML parser reads as a start tag, and what it reads
//! as that tag's attributes.
//!
//! tl reads every `<name ...>` as a tag, wherever it sits and however its attributes are
//! written. A stray `=` or quote earlier in the same tag makes a real HTML parser read the rest
//! of the tag differently: `<a href=b ="x>one</a><a href="y">two</a>` is, to html5ever's
//! tokenizer, one `<a>` tag ending at the first `>` (with `href` "b" and a dropped attribute
//! named `=`) followed by a second `<a href="y">`, while tl reads it as a single `<a>` whose
//! `href` runs to the second tag's closing `>`. [`scan`] reads `html` the way html5ever's
//! tokenizer does, steered by its own tree builder so raw-text elements and foreign content
//! (SVG, MathML) are handled the same way a browser handles them, and reports each real start
//! tag's span, attributes and whether it is self-closing.
//!
//! This is a narrower version of the html5ever-backed tag scan proposed for the markdown
//! pre-pass in the open `fix/tag-ends-from-html5ever` stack (#123, #232, #292): only the span
//! and attributes link extraction needs, with no raw-text ranges or `<base href>` tracking.
//! When that stack lands, converge the two rather than keeping both.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeBuilder, TreeBuilderOpts, TreeSink};
use html5ever::{Attribute, LocalName, QualName, TokenizerResult};
use memchr::{memchr, memchr2_iter};

/// The start tags an HTML parser reads as tags, in document order, each with every attribute it
/// reads on the tag.
pub(super) struct RealTags {
    tags: Vec<RealTag>,
    attrs: Vec<Attribute>,
}

/// A start tag: its span in the source, whether it is self-closing, and its attributes in
/// [`RealTags::attrs`]. [`scan`] is always called with one element name to keep, so a kept tag's
/// name is already known to its caller and is not stored again here.
struct RealTag {
    span: Range<usize>,
    self_closing: bool,
    attrs: Range<usize>,
}

/// A start tag as an HTML parser reads it.
pub(super) struct StartTag<'t> {
    /// The tag's bytes in the source, from its `<` to just past its `>`.
    pub(super) span: Range<usize>,
    /// Whether the tag ends in `/>`.
    pub(super) self_closing: bool,
    /// The attributes in source order, values decoded, with no repeated name (an HTML parser
    /// drops every copy of an attribute name after its first on the same tag).
    pub(super) attrs: &'t [Attribute],
}

impl RealTags {
    /// Every start tag, in document order. The spans do not overlap.
    pub(super) fn iter(&self) -> impl Iterator<Item = StartTag<'_>> {
        self.tags.iter().map(|tag| StartTag {
            span: tag.span.clone(),
            self_closing: tag.self_closing,
            attrs: &self.attrs[tag.attrs.clone()],
        })
    }
}

/// Read `html` as an HTML parser does, with scripting off (a crawler that runs no script fetches
/// a page, so `<noscript>` content is markup, as [`super::raw_text::mask_raw_text_markup`] also
/// assumes). Each start tag it reads as a tag is kept, with its span and attributes, when
/// `keep(element)` holds for its lower-cased name.
///
/// ~keep The tokenizer reports no source offsets, so the input is fed in pieces: each `<` alone,
/// ~keep and the bytes between, cut after every `>`. A start tag is emitted while the piece
/// ~keep holding its closing `>` is fed, so the offset fed so far is the end of that tag, and its
/// ~keep start is the first `<` at or after the end of the previous token (see
/// ~keep [`Recorder::next_start`]).
pub(super) fn scan(html: &str, keep: fn(&str) -> bool) -> RealTags {
    let options = TreeBuilderOpts {
        scripting_enabled: false,
        ..TreeBuilderOpts::default()
    };
    let sink = Recorder {
        tree: TreeBuilder::new(Names::default(), options),
        html,
        piece: Cell::new(0..0),
        next_start: Cell::new(0),
        keep,
        found: RefCell::new(RealTags {
            tags: Vec::new(),
            attrs: Vec::new(),
        }),
    };
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let input = BufferQueue::default();
    let feed = |piece: Range<usize>| {
        if piece.is_empty() {
            return;
        }
        input.push_back(StrTendril::from(&html[piece.clone()]));
        tokenizer.sink.piece.set(piece);
        while let TokenizerResult::Script(_) = tokenizer.feed(&input) {}
    };
    // ~keep Each `<` is fed on its own, and every other run of bytes is fed up to and including
    // ~keep the next `>`: this is what lets `Recorder::after_token` tell a piece that is a lone
    // ~keep `<` from one that ends a tag, which `keep_start_tag` and `next_start` both rely on.
    let mut from = 0;
    for at in memchr2_iter(b'<', b'>', html.as_bytes()) {
        if html.as_bytes()[at] == b'<' {
            feed(from..at);
            feed(at..at + 1);
        } else {
            feed(from..at + 1);
        }
        from = at + 1;
    }
    feed(from..html.len());
    tokenizer.end();
    tokenizer.sink.found.into_inner()
}

/// Forwards every token to the tree builder, which steers the tokenizer, and records each start
/// tag whose element is kept.
struct Recorder<'h> {
    tree: TreeBuilder<Rc<Node>, Names>,
    html: &'h str,
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
        let Some(start) = memchr(b'<', &self.html.as_bytes()[from..end]).map(|offset| from + offset) else {
            debug_assert!(false, "no `<` before the start tag ending at {end}");
            return;
        };
        let found = &mut *self.found.borrow_mut();
        let first = found.attrs.len();
        found.attrs.extend(tag.attrs.iter().cloned());
        found.tags.push(RealTag {
            span: start..end,
            self_closing: tag.self_closing,
            attrs: first..found.attrs.len(),
        });
    }

    /// Move [`Self::next_start`] past a token that was just emitted.
    fn after_token(&self) {
        let piece = self.piece.take();
        let lone_lt = &self.html.as_bytes()[piece.clone()] == b"<";
        self.next_start.set(if lone_lt { piece.start } else { piece.end });
        self.piece.set(piece);
    }
}

impl TokenSink for Recorder<'_> {
    type Handle = Rc<Node>;

    fn process_token(&self, token: Token, line_number: u64) -> TokenSinkResult<Rc<Node>> {
        if let Token::TagToken(tag) = &token
            && tag.kind == TagKind::StartTag
        {
            self.keep_start_tag(tag);
        }
        if !matches!(token, Token::ParseError(_)) {
            self.after_token();
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

/// A node of the tree the builder keeps: only what it asks about, the element name and whether a
/// MathML `annotation-xml` is an HTML integration point. No tree is actually kept.
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

/// A tree sink that keeps no tree: the builder needs element names to track the open elements
/// for raw-text, foreign-content and integration-point handling, and nothing else here is read.
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

    fn parse_error(&self, _msg: std::borrow::Cow<'static, str>) {}

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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use proptest::prelude::*;

    use super::*;

    /// The start and end of each `<a>` start tag in `html`.
    fn link_spans(html: &str) -> Vec<(usize, usize)> {
        scan(html, |name| name == "a")
            .iter()
            .map(|tag| (tag.span.start, tag.span.end))
            .collect()
    }

    #[test]
    fn finds_each_well_formed_tag_span() {
        assert_eq!(
            link_spans(r#"<a href="x">one</a> <a href="y">two</a>"#),
            [(0, 12), (20, 32)]
        );
    }

    #[test]
    fn a_stray_equals_and_quote_end_the_first_tag_at_the_next_gt() {
        // ~keep Matches #292's own corpus fixture for this exact ambiguity.
        assert_eq!(
            link_spans(r#"<a href=b ="x>one</a><a href="y">two</a>"#),
            [(0, 14), (21, 33)]
        );
    }

    #[test]
    fn a_tag_inside_title_text_is_not_a_real_tag() {
        assert_eq!(link_spans("<title><a href=1></title><a href=2>x</a>"), [(25, 35)]);
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

    /// The `<a>` start tags [`scan`] reports whose span, read on its own, is not exactly the one
    /// whole start tag [`scan`] itself says is there.
    ///
    /// ~keep Ported from the html5ever tag-scan stack's own review corpus (#123's review), which
    /// ~keep this module's span-finding (`Recorder::next_start`, `keep_start_tag`) matches.
    fn misread_spans(html: &str) -> Vec<(Range<usize>, String)> {
        // ~keep Only `<a>` is kept: this module's `StartTag` does not carry a name (every
        // ~keep production caller already knows it, from the one name it asked `scan` to keep),
        // ~keep so a generic multi-element corpus would have nothing to compare a found tag's
        // ~keep name against.
        scan(html, |name| name == "a")
            .iter()
            .filter(|tag| {
                let attrs = tag
                    .attrs
                    .iter()
                    .map(|attr| (attr.name.local.to_string(), attr.value.to_string()))
                    .collect();
                let read = (String::from("a"), attrs, tag.self_closing);
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
        // ~keep The corpus from the review of #123, to four bytes: `<a href=1 {s}>` with `s` over
        // ~keep the bytes that move a tag's end, after a character reference and a comment that
        // ~keep each hold a `<a`. (#123's own corpus also covers CDATA inside `<svg>`, which this
        // ~keep module never sees the tag of, since it only ever scans for `<a>`.)
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
            .flat_map(|s| misread_spans(&format!("x&amp<a href=1 {s}>z</a><!--<a -->&#<a href=2>w</a>")))
            .collect();
        assert!(
            misread.is_empty(),
            "{} tags misread, first: {:?}",
            misread.len(),
            misread.first()
        );
    }

    proptest! {
        /// Each `<a>` start tag the scan reports is, read on its own, one whole start tag with
        /// the same attributes and self-closing flag.
        #[test]
        fn each_start_tag_reads_alone_as_the_same_tag(
            html in r#"(<a href=x>|<a b=c ="d>|<a title="<a x>">|<title>|</title>|&amp|&#|&|<3|<<|[a-z0-9 <>"'/=\t-]){0,40}"#
        ) {
            let misread = misread_spans(&html);
            prop_assert!(misread.is_empty(), "misread: {:?}", misread);
        }
    }

    #[test]
    fn keeps_the_slash_of_a_self_closing_tag() {
        let tags = scan(r#"<svg><a href="x"/></svg>"#, |name| name == "a");
        let tag = tags.iter().next().expect("one tag");
        assert!(tag.self_closing);
    }

    #[test]
    fn an_html_parser_drops_a_repeated_attribute_name() {
        let tags = scan(r#"<a href="a.html" href="b.html">x</a>"#, |name| name == "a");
        let tag = tags.iter().next().expect("one tag");
        assert_eq!(tag.attrs.len(), 1);
        assert_eq!(&*tag.attrs[0].value, "a.html");
    }
}
