use crawlberg::{ContentConfig, CrawlConfig, create_engine, scrape};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn markdown_with(html: &str, options: Value) -> String {
    let content: ContentConfig = serde_json::from_value(options).expect("valid content options");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(html)
                .insert_header("content-type", "text/html; charset=utf-8"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let config = CrawlConfig {
        content: ContentConfig {
            extract_metadata: false,
            ..content
        },
        ..CrawlConfig::builder()
            .allow_private_networks(true)
            .respect_robots_txt(false)
            .build()
    };
    let engine = create_engine(Some(config)).expect("engine builds");
    let result = scrape(&engine, &server.uri()).await.expect("scrape succeeds");
    let markdown = result.markdown.expect("HTML has markdown").content;
    server.verify().await;
    markdown
}

#[tokio::test]
async fn should_omit_abbreviation_title_when_expansion_is_disabled() {
    let html = r#"<p>The <abbr title="World Health Organization">WHO</abbr> said so.</p>"#;
    assert_eq!(
        markdown_with(html, json!({"expand_abbreviations": false})).await,
        "The WHO said so.\n"
    );
}

#[tokio::test]
async fn should_escape_literal_asterisks_when_requested() {
    assert_eq!(
        markdown_with(
            "<p>a size of 350*250 (w*h) is fine</p>",
            json!({"escape_asterisks": true})
        )
        .await,
        concat!(r"a size of 350\*250 (w\*h) is fine", "\n")
    );
}

#[tokio::test]
async fn should_escape_literal_backticks_and_brackets_when_requested() {
    assert_eq!(
        markdown_with("<p>I`am alpha [beta](gamma) delta</p>", json!({"escape_misc": true})).await,
        concat!(r"I\`am alpha \[beta\](gamma) delta", "\n")
    );
}

#[tokio::test]
async fn should_preserve_converter_markup_when_literal_escaping_is_requested() {
    let cases = [
        (
            "<p>literal *star* <em>emphasis</em> <code>code</code></p>",
            concat!(r"literal \*star\* *emphasis* `code`", "\n"),
        ),
        (
            "<p>I`am [beta](gamma) 350*250 <em>emphasis</em> <code>code</code></p>",
            concat!(r"I\`am \[beta\](gamma) 350\*250 *emphasis* `code`", "\n"),
        ),
    ];
    for (html, expected) in cases {
        assert_eq!(
            markdown_with(html, json!({"escape_misc": true, "escape_asterisks": true})).await,
            expected
        );
    }
}

#[tokio::test]
async fn should_keep_existing_abbreviation_and_literal_defaults() {
    assert_eq!(
        markdown_with(
            r#"<p>The <abbr title="World Health Organization">WHO</abbr>: 350*250 I`am [beta](gamma).</p>"#,
            json!({}),
        )
        .await,
        "The WHO (World Health Organization): 350*250 I`am [beta](gamma).\n"
    );
}

#[test]
fn should_preserve_defaults_and_round_trip_conversion_options() {
    let defaults = serde_json::to_value(ContentConfig::default()).expect("serializable config");
    assert_eq!(defaults["expand_abbreviations"], json!(true));
    assert_eq!(defaults["escape_misc"], json!(false));
    assert_eq!(defaults["escape_asterisks"], json!(false));
    let options = json!({"expand_abbreviations": false, "escape_misc": true, "escape_asterisks": true});
    let content: ContentConfig = serde_json::from_value(options.clone()).expect("valid options");
    let round_trip = serde_json::to_value(content).expect("serializable config");
    for field in ["expand_abbreviations", "escape_misc", "escape_asterisks"] {
        assert_eq!(round_trip[field], options[field], "{field}");
    }
}

#[test]
fn should_reject_non_boolean_conversion_options() {
    for field in ["expand_abbreviations", "escape_misc", "escape_asterisks"] {
        let error =
            serde_json::from_value::<ContentConfig>(json!({(field): "true"})).expect_err("a string is not a boolean");
        assert!(error.to_string().contains("expected a boolean"), "{field}: {error}");
    }
}

#[tokio::test]
async fn should_preserve_space_inside_a_whitespace_only_link() {
    assert_eq!(
        markdown_with(
            "<p>alpha<a href=\"https://example.com/x\"><span> </span></a>omega</p>",
            json!({}),
        )
        .await,
        "alpha [](https://example.com/x)omega\n"
    );
}

#[tokio::test]
async fn should_preserve_no_break_space_inside_a_whitespace_only_link() {
    assert_eq!(
        markdown_with(
            "<p>alpha<a href=\"https://example.com/x\"><span>&nbsp;</span></a>omega</p>",
            json!({}),
        )
        .await,
        "alpha [](https://example.com/x)omega\n"
    );
}

#[tokio::test]
async fn should_preserve_newline_inside_a_whitespace_only_link() {
    assert_eq!(
        markdown_with(
            "<p>alpha<a href=\"https://example.com/x\"><span>\n</span></a>omega</p>",
            json!({}),
        )
        .await,
        "alpha [](https://example.com/x)omega\n"
    );
}

#[tokio::test]
async fn should_preserve_tab_inside_a_whitespace_only_link() {
    assert_eq!(
        markdown_with(
            "<p>alpha<a href=\"https://example.com/x\"><span>\t</span></a>omega</p>",
            json!({}),
        )
        .await,
        "alpha [](https://example.com/x)omega\n"
    );
}
