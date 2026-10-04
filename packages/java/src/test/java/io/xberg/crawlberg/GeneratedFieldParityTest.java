package io.xberg.crawlberg;

import static org.assertj.core.api.Assertions.assertThat;

import java.nio.file.Path;
import java.util.List;
import org.junit.jupiter.api.Test;

final class GeneratedFieldParityTest {
  @Test
  void generatedConfigFieldsRemainConstructible() {
    final Path chromePath = Path.of("/opt/chromium");
    final List<String> chromeArgs = List.of("--headless=new", "--disable-gpu");

    final BrowserConfig browserConfig = BrowserConfig.builder()
                                            .withChromePath(chromePath)
                                            .withChromeArgs(chromeArgs)
                                            .build();
    final CrawlConfig crawlConfig =
        CrawlConfig.builder().withPathPatternsMatchUrl(true).build();

    assertThat(browserConfig.chromePath()).isEqualTo(chromePath);
    assertThat(browserConfig.chromeArgs())
        .containsExactlyElementsOf(chromeArgs);
    assertThat(crawlConfig.pathPatternsMatchUrl()).isTrue();
  }

  @Test
  void generatedRobotsResultFieldsRemainConstructible() {
    final CrawlPageResult crawlPage = CrawlPageResult.builder()
                                          .withNoindexDetected(true)
                                          .withNofollowDetected(false)
                                          .build();
    final ScrapeResult scrapeResult = ScrapeResult.builder()
                                          .withNoindexDetected(false)
                                          .withNofollowDetected(true)
                                          .build();

    assertThat(crawlPage.noindexDetected()).isTrue();
    assertThat(crawlPage.nofollowDetected()).isFalse();
    assertThat(scrapeResult.noindexDetected()).isFalse();
    assertThat(scrapeResult.nofollowDetected()).isTrue();
  }
}
