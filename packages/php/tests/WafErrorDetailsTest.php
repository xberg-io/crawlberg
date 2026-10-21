<?php

declare(strict_types=1);

use Crawlberg\CrawlbergApi;
use Crawlberg\CrawlConfig;
use Crawlberg\CrawlError;
use Crawlberg\WafBlockedError;
use PHPUnit\Framework\TestCase;

final class WafErrorDetailsTest extends TestCase
{
    public function testScrapePreservesTypedWafDetails(): void
    {
        $listener = stream_socket_server('tcp://127.0.0.1:0', $errno, $error);
        self::assertIsResource($listener);
        $address = stream_socket_get_name($listener, false);
        fclose($listener);
        self::assertIsString($address);
        $router = tempnam(sys_get_temp_dir(), 'crawlberg-waf-');
        self::assertIsString($router);
        $body = '<html><body><h1>Checking your browser</h1></body></html>';
        file_put_contents(
            $router,
            '<?php http_response_code(403); header("cf-mitigated: challenge"); '
            . 'header("Server: cloudflare"); header("Content-Type: text/html"); echo '
            . var_export($body, true)
            . ';',
        );
        $server = proc_open(
            [PHP_BINARY, '-n', '-S', $address, $router],
            [
                0 => ['pipe', 'r'],
                1 => ['file', '/dev/null', 'a'],
                2 => ['file', '/dev/null', 'a'],
            ],
            $pipes,
        );
        self::assertIsResource($server);
        try {
            for ($attempt = 0; $attempt < 100; ++$attempt) {
                $ready = @stream_socket_client('tcp://' . $address, $errno, $error, 0.1);
                if (is_resource($ready)) {
                    fclose($ready);
                    break;
                }
                usleep(20_000);
            }
            self::assertLessThan(100, $attempt, 'Local blocked origin must be serving requests');
            $config = CrawlConfig::from_json(json_encode([
                'browser' => ['mode' => 'never'],
                'ssrf' => ['denyPrivate' => false],
                'respectRobotsTxt' => false,
                'retryCount' => 0,
                'rateLimitMs' => 0,
            ], JSON_THROW_ON_ERROR));
            $engine = CrawlbergApi::createEngine($config);
            try {
                CrawlbergApi::scrape($engine, 'http://' . $address . '/');
                self::fail('A blocked origin must raise WafBlockedError');
            } catch (WafBlockedError $caught) {
                self::assertInstanceOf(CrawlError::class, $caught);
                self::assertSame('cloudflare', $caught->vendor);
                self::assertSame(403, $caught->status);
                self::assertSame($body, $caught->body);
                self::assertStringContainsString('cloudflare', $caught->getMessage());
            }
        } finally {
            fclose($pipes[0]);
            proc_terminate($server);
            proc_close($server);
            unlink($router);
        }
    }
}
