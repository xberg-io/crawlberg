package io.xberg.crawlberg;

import static org.assertj.core.api.Assertions.assertThat;

import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

final class ReleasedConstructorCompatibilityTest {
  @Test
  void shouldPreserveReleasedContentConstructorAndDefaultNewOptions() {
    final ContentConfig config = new ContentConfig(
        "markdown", "standard", false, false, List.of("script"),
        List.of("span"), List.of("noscript"), true, 7L, true, 100L, false,
        false, true, "+", 4L, "_", true, List.of("td"), true);

    assertThat(config.outputFormat()).isEqualTo("markdown");
    assertThat(config.removeNavigation()).isFalse();
    assertThat(config.stripTags()).containsExactly("script");
    assertThat(config.maxDepth()).isEqualTo(7L);
    assertThat(config.wrapWidth()).isEqualTo(100L);
    assertThat(config.stripNewlines()).isTrue();
    assertThat(config.bullets()).isEqualTo("+");
    assertThat(config.listIndentWidth()).isEqualTo(4L);
    assertThat(config.strongEmSymbol()).isEqualTo("_");
    assertThat(config.compactTables()).isTrue();
    assertThat(config.keepInlineImagesIn()).containsExactly("td");
    assertThat(config.keepHidden()).isTrue();
    assertThat(config.expandAbbreviations()).isTrue();
    assertThat(config.escapeMisc()).isFalse();
    assertThat(config.escapeAsterisks()).isFalse();
    assertThat(config.includeIconLabels()).isTrue();
  }

  @Test
  void shouldPreserveReleasedCrawlConstructorAndDefaultHeaderAllowlist() {
    final Map<String, String> headers = Map.of("X-Client", "released-client");
    final CrawlConfig config = new CrawlConfig(
        3L, 20L, 10L, 2L, CrawlStrategyKind.BFS, null, null, null, true, false,
        "released-client", true, true, List.of("/docs"), List.of("/private"),
        false, true, true, false, List.of("utm_source"), headers, 5000L, 50L,
        4L, 2L, List.of((short)503), 100L, 1000L, 0.25, true, null, 4096L,
        List.of("script"), null, 100L, "docs", false, List.of(), null, null,
        null, List.of("secondary-client"), false, true, 2, true, 8192L,
        List.of("application/pdf"), null, null, null, "saved-profile", true,
        null, true, true);

    assertThat(config.maxDepth()).isEqualTo(3L);
    assertThat(config.userAgent()).isEqualTo("released-client");
    assertThat(config.pathPatternsMatchUrl()).isTrue();
    assertThat(config.customHeaders()).isEqualTo(headers);
    assertThat(config.retryCodes()).containsExactly((short)503);
    assertThat(config.rateLimitJitterRatio()).isEqualTo(0.25);
    assertThat(config.browserProfile()).isEqualTo("saved-profile");
    assertThat(config.saveBrowserProfile()).isTrue();
    assertThat(config.ssrfDenyPrivateExplicit()).isTrue();
    assertThat(config.dangerAcceptInvalidCerts()).isTrue();
    assertThat(config.subdomainHeaderAllowlist()).isNotNull().isEmpty();
  }
}
