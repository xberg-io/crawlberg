import 'package:crawlberg/crawlberg.dart';
import 'package:test/test.dart';

void main() {
  test('an old named content constructor uses false for the new field', () {
    const config = ContentConfig(
      outputFormat: 'markdown',
      preprocessingPreset: 'minimal',
      removeNavigation: false,
      removeForms: false,
      stripTags: [],
      preserveTags: [],
      excludeSelectors: [],
      skipImages: true,
      maxDepth: 5,
      wrap: false,
      wrapWidth: 80,
      includeDocumentStructure: true,
      extractMetadata: false,
    );
    expect(config.skipImages, isTrue);
    expect(config.maxDepth, 5);
    expect(config.keepHidden, isFalse);
  });
}
