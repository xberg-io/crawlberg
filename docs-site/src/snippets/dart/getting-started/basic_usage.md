<!-- snippet:skip reason="alef bug: DartValidator::is_dependency_error matches the lowercase diagnostic codes `dart analyze` prints in its human format, but the batch path runs `--format=machine`, which prints them uppercase (URI_DOES_NOT_EXIST). The unresolved `package:crawlberg` import is therefore classified as a snippet defect and hard-fails instead of passing at syntax level. Restore validation with a `[workspace.docs.snippets.sessions.dart]` session (cwd packages/dart, manifest pubspec.yaml, before `dart pub get`, env PUB_CACHE) -- verified to PASS at compile -- once the repo takes the full-tree alef.toml hash restamp that any alef.toml edit forces." -->
```dart title="Dart"
import 'package:crawlberg/crawlberg.dart';
import 'package:crawlberg/src/crawlberg_bridge_generated/frb_generated.dart'
    show RustLib;

Future<void> main() async {
  await RustLib.init();

  // Simplest case: scrape a single page with default settings.
  final engine = await CrawlbergBridge.createEngine();
  final result = await CrawlbergBridge.scrape(engine, 'https://example.com/');
  print('Title: ${result.metadata.title ?? ''}');
  print('Status: ${result.statusCode}');
  print('Links found: ${result.links.length}');

  // Crawl from a seed URL, limited to one hop and a handful of pages.
  final crawlConfig = await createCrawlConfigFromJson(
    json: r'{"max_depth":1,"max_pages":5}',
  );
  final crawlEngine = await CrawlbergBridge.createEngine(config: crawlConfig);
  final crawlResult = await CrawlbergBridge.crawl(
    crawlEngine,
    'https://en.wikipedia.org/wiki/Web_scraping',
  );
  print('Pages crawled: ${crawlResult.pages.length}');
}
```
