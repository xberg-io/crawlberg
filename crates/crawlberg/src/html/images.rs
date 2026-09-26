//! Image extraction from HTML documents.

use std::borrow::Cow;

use tl::VDom;
use url::Url;

use crate::types::{ImageInfo, ImageSource};

use super::link_targets::srcset_candidates;
use super::selectors::{SEL_IMG_SRC, SEL_META, SEL_SOURCE_SRCSET};
use super::{attr_eq, clean_url, get_attr, get_url_attr, has_scheme, resolve_url};

/// Extract all images from a parsed HTML document, resolved against the document's base URL.
///
/// Sources are appended in a fixed order — `<img>`, `<picture><source>`, `og:image`,
/// `twitter:image` — and downstream dedup depends on it. ~keep
pub(crate) fn extract_images(dom: &VDom<'_>, base_url: &Url) -> Vec<ImageInfo> {
    let mut images = Vec::new();
    collect_img_elements(dom, base_url, &mut images);
    collect_picture_sources(dom, base_url, &mut images);
    collect_meta_images(
        dom,
        base_url,
        "property",
        "og:image",
        &ImageSource::OgImage,
        &mut images,
    );
    collect_meta_images(
        dom,
        base_url,
        "name",
        "twitter:image",
        &ImageSource::TwitterImage,
        &mut images,
    );
    images
}

/// Collect `<img src>` images, skipping blank and inline `data:` sources.
fn collect_img_elements(dom: &VDom<'_>, base_url: &Url, images: &mut Vec<ImageInfo>) {
    let parser = dom.parser();
    let Some(iter) = dom.query_selector(SEL_IMG_SRC) else {
        return;
    };
    for handle in iter {
        let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
            continue;
        };
        let Some(src) = get_url_attr(tag, "src") else {
            continue;
        };
        if has_scheme(&src, "data") {
            continue;
        }
        images.push(ImageInfo {
            url: resolve_url(&src, base_url),
            alt: get_attr(tag, "alt").map(Cow::into_owned),
            width: get_attr(tag, "width").and_then(|w| w.parse::<u32>().ok()),
            height: get_attr(tag, "height").and_then(|h| h.parse::<u32>().ok()),
            source: ImageSource::Img,
        });
    }
}

/// Collect the first candidate of each `<source srcset>`, dropping its density descriptor and
/// skipping blank and inline `data:` candidates.
fn collect_picture_sources(dom: &VDom<'_>, base_url: &Url, images: &mut Vec<ImageInfo>) {
    let parser = dom.parser();
    let Some(iter) = dom.query_selector(SEL_SOURCE_SRCSET) else {
        return;
    };
    for handle in iter {
        let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
            continue;
        };
        let srcset = get_attr(tag, "srcset").unwrap_or_default();
        let Some(raw_url) = srcset_candidates(&srcset)
            .next()
            .and_then(|(url, _)| clean_url(Cow::Borrowed(url)))
        else {
            continue;
        };
        if has_scheme(&raw_url, "data") {
            continue;
        }
        images.push(ImageInfo {
            url: resolve_url(&raw_url, base_url),
            alt: None,
            width: None,
            height: None,
            source: ImageSource::PictureSource,
        });
    }
}

/// Collect images from the `content` of each `<meta>` whose `attr` is `name`, in any case,
/// skipping inline `data:` contents.
fn collect_meta_images(
    dom: &VDom<'_>,
    base_url: &Url,
    attr: &str,
    name: &str,
    source: &ImageSource,
    images: &mut Vec<ImageInfo>,
) {
    let parser = dom.parser();
    let Some(iter) = dom.query_selector(SEL_META) else {
        return;
    };
    for handle in iter {
        let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
            continue;
        };
        if !attr_eq(tag, attr, name) {
            continue;
        }
        let Some(content) = get_url_attr(tag, "content") else {
            continue;
        };
        if has_scheme(&content, "data") {
            continue;
        }
        images.push(ImageInfo {
            url: resolve_url(&content, base_url),
            alt: None,
            width: None,
            height: None,
            source: source.clone(),
        });
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    /// Flattened `ImageInfo` used so a whole extraction can be compared in one
    /// assertion without requiring `PartialEq` on the public type.
    type Flat = (String, Option<String>, Option<u32>, Option<u32>, String);

    fn extract(html: &str) -> Vec<Flat> {
        let dom = crate::html::parse_html(html).expect("valid HTML");
        let base_url = Url::parse("https://example.com/dir/page.html").expect("valid base URL");
        extract_images(&dom, &base_url)
            .into_iter()
            .map(|i| (i.url, i.alt, i.width, i.height, i.source.to_string()))
            .collect()
    }

    fn flat(url: &str, source: &str) -> Flat {
        (url.to_owned(), None, None, None, source.to_owned())
    }

    #[test]
    fn img_tags_keep_alt_and_parsed_dimensions() {
        assert_eq!(
            extract(r#"<img src="a.png" alt="A" width="10" height="20">"#),
            vec![(
                "https://example.com/dir/a.png".to_owned(),
                Some("A".to_owned()),
                Some(10),
                Some(20),
                "img".to_owned()
            )]
        );
    }

    #[test]
    fn img_dimensions_that_do_not_parse_as_u32_become_none() {
        assert_eq!(
            extract(r#"<img src="a.png" width="10px" height="-3">"#),
            vec![(
                "https://example.com/dir/a.png".to_owned(),
                None,
                None,
                None,
                "img".to_owned()
            )]
        );
    }

    #[test]
    fn img_with_empty_or_data_src_is_skipped() {
        assert_eq!(extract(r#"<img src="" alt="empty">"#), Vec::<Flat>::new());
        assert_eq!(
            extract(r#"<img src="data:image/png;base64,iVBOR" alt="inline">"#),
            Vec::<Flat>::new()
        );
    }

    #[test]
    fn unresolvable_src_falls_back_to_the_raw_value() {
        assert_eq!(extract(r#"<img src="http://[bad">"#), vec![flat("http://[bad", "img")]);
    }

    #[test]
    fn srcset_takes_the_first_candidate_without_its_descriptor() {
        assert_eq!(
            extract(r#"<source srcset="a.png 1x, b.png 2x">"#),
            vec![flat("https://example.com/dir/a.png", "picture_source")]
        );
    }

    #[test]
    fn srcset_that_is_empty_or_only_separators_yields_nothing() {
        assert_eq!(extract(r#"<source srcset="">"#), Vec::<Flat>::new());
        assert_eq!(extract("<source srcset=\" , \t,\">"), Vec::<Flat>::new());
    }

    #[test]
    fn srcset_skips_leading_separators_as_a_browser_does() {
        assert_eq!(
            extract(r#"<source srcset=" , b.png">"#),
            vec![flat("https://example.com/dir/b.png", "picture_source")]
        );
    }

    #[test]
    fn meta_images_are_resolved_and_empty_content_is_skipped() {
        assert_eq!(
            extract(r#"<meta property="og:image" content="og.png">"#),
            vec![flat("https://example.com/dir/og.png", "og:image")]
        );
        assert_eq!(
            extract(r#"<meta name="twitter:image" content="tw.png">"#),
            vec![flat("https://example.com/dir/tw.png", "twitter:image")]
        );
        assert_eq!(extract(r#"<meta property="og:image" content="">"#), Vec::<Flat>::new());
        assert_eq!(extract(r#"<meta name="twitter:image" content="">"#), Vec::<Flat>::new());
    }

    #[test]
    fn sources_are_emitted_in_img_srcset_og_twitter_order() {
        let html = concat!(
            r#"<meta name="twitter:image" content="tw.png">"#,
            r#"<meta property="og:image" content="og.png">"#,
            r#"<source srcset="s.png">"#,
            r#"<img src="i1.png"><img src="i2.png">"#,
        );
        assert_eq!(
            extract(html),
            vec![
                flat("https://example.com/dir/i1.png", "img"),
                flat("https://example.com/dir/i2.png", "img"),
                flat("https://example.com/dir/s.png", "picture_source"),
                flat("https://example.com/dir/og.png", "og:image"),
                flat("https://example.com/dir/tw.png", "twitter:image"),
            ]
        );
    }
}
