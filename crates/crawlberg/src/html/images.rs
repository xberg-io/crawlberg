//! Image extraction from HTML documents.

use std::borrow::Cow;

use tl::VDom;
use url::Url;

use crate::types::{ImageInfo, ImageSource};

use super::links::effective_base_url;
use super::selectors::{SEL_IMG_SRC, SEL_OG_IMAGE, SEL_SOURCE_SRCSET, SEL_TWITTER_IMAGE};
use super::{get_attr, resolve_url};

/// Extract all images from a parsed HTML document.
///
/// Relative addresses resolve against the same base as the links list: the first `<base href>`,
/// else `document_url`.
///
/// Sources are appended in a fixed order — `<img>`, `<picture><source>`, `og:image`,
/// `twitter:image` — and downstream dedup depends on it. ~keep
pub(crate) fn extract_images(dom: &VDom<'_>, document_url: &Url) -> Vec<ImageInfo> {
    let base_url = &effective_base_url(dom, document_url);
    let mut images = Vec::new();
    collect_img_elements(dom, base_url, &mut images);
    collect_picture_sources(dom, base_url, &mut images);
    collect_meta_images(dom, base_url, SEL_OG_IMAGE, &ImageSource::OgImage, &mut images);
    collect_meta_images(
        dom,
        base_url,
        SEL_TWITTER_IMAGE,
        &ImageSource::TwitterImage,
        &mut images,
    );
    images
}

/// Collect `<img src>` images, skipping empty and inline `data:` sources.
fn collect_img_elements(dom: &VDom<'_>, base_url: &Url, images: &mut Vec<ImageInfo>) {
    let parser = dom.parser();
    let Some(iter) = dom.query_selector(SEL_IMG_SRC) else {
        return;
    };
    for handle in iter {
        let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
            continue;
        };
        let src = get_attr(tag, "src").unwrap_or_default();
        if src.is_empty() || src.starts_with("data:") {
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

/// Collect the first candidate of each `<source srcset>`, dropping its density descriptor.
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
        if srcset.is_empty() {
            continue;
        }
        let first_url = srcset.split(',').next().unwrap_or("").trim();
        let raw_url = first_url.split_whitespace().next().unwrap_or("");
        if raw_url.is_empty() {
            continue;
        }
        images.push(ImageInfo {
            url: resolve_url(raw_url, base_url),
            alt: None,
            width: None,
            height: None,
            source: ImageSource::PictureSource,
        });
    }
}

/// Collect images from `<meta ... content>` tags matched by `selector`.
fn collect_meta_images(
    dom: &VDom<'_>,
    base_url: &Url,
    selector: &str,
    source: &ImageSource,
    images: &mut Vec<ImageInfo>,
) {
    let parser = dom.parser();
    let Some(iter) = dom.query_selector(selector) else {
        return;
    };
    for handle in iter {
        let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
            continue;
        };
        let Some(content) = get_attr(tag, "content") else {
            continue;
        };
        if content.is_empty() {
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
    fn srcset_that_is_empty_or_descriptor_only_yields_nothing() {
        assert_eq!(extract(r#"<source srcset="">"#), Vec::<Flat>::new());
        assert_eq!(extract(r#"<source srcset=" , b.png">"#), Vec::<Flat>::new());
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
