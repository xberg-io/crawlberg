package io.xberg.crawlberg;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;
import org.junit.jupiter.api.Test;

class ContentConstructorCompatibilityTest {
  @Test
  void shouldPreserveTheThirteenArgumentContentConstructor() {
    var config = new ContentConfig("markdown", "minimal", false, false,
                                   List.of(), List.of(), List.of(), true, 5L,
                                   false, 80L, true, false);
    assertTrue(config.skipImages());
    assertEquals(5L, config.maxDepth());
    assertFalse(config.extractMetadata());
    assertFalse(config.keepHidden());
  }
}
