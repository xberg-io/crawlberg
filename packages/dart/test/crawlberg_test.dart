import 'package:test/test.dart';
import 'package:crawlberg/crawlberg.dart' as crawlberg;

void main() {
  test('HreflangEntry equality holds for identical field values', () {
    // Literal-constructs the generated `HreflangEntry` DTO twice with identical field
    // values and compares them for equality, so a constructor that drops/renames a
    // field, or generated equality that stops being field-based, fails `dart test`
    // immediately instead of shipping green with a suite that asserts nothing about
    // the generated API. Create-only scaffold seed. ~keep
    final a = crawlberg.HreflangEntry(lang: 'alef-scaffold', url: 'alef-scaffold');
    final b = crawlberg.HreflangEntry(lang: 'alef-scaffold', url: 'alef-scaffold');
    expect(a, equals(b));
  });
}
