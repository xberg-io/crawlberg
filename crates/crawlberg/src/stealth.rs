//! Headless-chromium fingerprint patches, gated behind config.browser.stealth.
//!
//! Patches are injected via `Page.addScriptToEvaluateOnNewDocument` so they
//! run before the target page's own scripts on every navigation. Source
//! patterns are sketches of the well-known puppeteer-extra-plugin-stealth
//! patches — ported to bare JS here, no external runtime dependency.

use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;

/// JS source for the stealth patches. Concatenated string runs as a single
/// pre-document script. Each section is a self-invoking function with its
/// own try/catch so one failure doesn't take out the others.
const STEALTH_SCRIPT: &str = r#"
// 1. navigator.webdriver should be false (or absent), not true.
(function() {
    try {
        Object.defineProperty(Navigator.prototype, 'webdriver', {
            get: () => false,
            configurable: true,
        });
    } catch (_) {}
})();

// 2. navigator.plugins and navigator.mimeTypes should not be empty.
(function() {
    try {
        const fakePlugins = [
            { name: 'PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
            { name: 'Chrome PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
            { name: 'Chromium PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
            { name: 'Microsoft Edge PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
            { name: 'WebKit built-in PDF', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
        ];
        Object.defineProperty(Navigator.prototype, 'plugins', {
            get: () => fakePlugins,
            configurable: true,
        });
        Object.defineProperty(Navigator.prototype, 'mimeTypes', {
            get: () => [
                { type: 'application/pdf', suffixes: 'pdf', description: '' },
                { type: 'text/pdf', suffixes: 'pdf', description: '' },
            ],
            configurable: true,
        });
    } catch (_) {}
})();

// 3. chrome.runtime should be defined; headless chromium leaves it undefined.
(function() {
    try {
        if (typeof window.chrome === 'undefined' || window.chrome === null) {
            window.chrome = {};
        }
        if (typeof window.chrome.runtime === 'undefined') {
            window.chrome.runtime = {
                OnInstalledReason: { CHROME_UPDATE: 'chrome_update', INSTALL: 'install', SHARED_MODULE_UPDATE: 'shared_module_update', UPDATE: 'update' },
                OnRestartRequiredReason: { APP_UPDATE: 'app_update', OS_UPDATE: 'os_update', PERIODIC: 'periodic' },
                PlatformArch: { ARM: 'arm', ARM64: 'arm64', MIPS: 'mips', MIPS64: 'mips64', X86_32: 'x86-32', X86_64: 'x86-64' },
                PlatformNaclArch: { ARM: 'arm', MIPS: 'mips', MIPS64: 'mips64', X86_32: 'x86-32', X86_64: 'x86-64' },
                PlatformOs: { ANDROID: 'android', CROSS: 'cross', LINUX: 'linux', MAC: 'mac', OPENBSD: 'openbsd', WIN: 'win' },
                RequestUpdateCheckStatus: { NO_UPDATE: 'no_update', THROTTLED: 'throttled', UPDATE_AVAILABLE: 'update_available' },
            };
        }
    } catch (_) {}
})();

// 4. WebGL vendor + renderer spoof (common modern integrated GPU strings).
(function() {
    try {
        const SPOOF = {
            37445: 'Intel Inc.',          // UNMASKED_VENDOR_WEBGL
            37446: 'Intel Iris OpenGL Engine',  // UNMASKED_RENDERER_WEBGL
        };
        const getParameter = WebGLRenderingContext.prototype.getParameter;
        WebGLRenderingContext.prototype.getParameter = function(parameter) {
            if (SPOOF[parameter] !== undefined) return SPOOF[parameter];
            return getParameter.apply(this, arguments);
        };
        if (typeof WebGL2RenderingContext !== 'undefined') {
            const getParameter2 = WebGL2RenderingContext.prototype.getParameter;
            WebGL2RenderingContext.prototype.getParameter = function(parameter) {
                if (SPOOF[parameter] !== undefined) return SPOOF[parameter];
                return getParameter2.apply(this, arguments);
            };
        }
    } catch (_) {}
})();

// 5. Canvas + audio fingerprint with tiny per-page random offsets so the
// fingerprint isn't perfectly stable across requests but reads as "real".
(function() {
    try {
        const noise = (Math.random() * 0.0001) - 0.00005;
        const toDataURL = HTMLCanvasElement.prototype.toDataURL;
        HTMLCanvasElement.prototype.toDataURL = function(...args) {
            const ctx = this.getContext('2d');
            if (ctx) {
                const id = ctx.getImageData(0, 0, this.width, this.height);
                for (let i = 0; i < id.data.length; i += 4) {
                    id.data[i + 0] = id.data[i + 0] ^ ((Math.random() < 0.5) ? 0 : 1);
                }
                ctx.putImageData(id, 0, 0);
            }
            return toDataURL.apply(this, args);
        };
        if (typeof AnalyserNode !== 'undefined') {
            const orig = AnalyserNode.prototype.getFloatFrequencyData;
            AnalyserNode.prototype.getFloatFrequencyData = function(array) {
                orig.apply(this, arguments);
                for (let i = 0; i < array.length; i++) {
                    array[i] = array[i] + noise;
                }
            };
        }
    } catch (_) {}
})();

// 6. Permissions.query — Notification.permission should report 'default'.
(function() {
    try {
        if (!window.Notification) window.Notification = { permission: 'default' };
        if (navigator.permissions && navigator.permissions.query) {
            const origQuery = navigator.permissions.query.bind(navigator.permissions);
            navigator.permissions.query = (parameters) => {
                if (parameters && parameters.name === 'notifications') {
                    return Promise.resolve({ state: window.Notification.permission, name: 'notifications' });
                }
                return origQuery(parameters);
            };
        }
    } catch (_) {}
})();
    "#;

/// Return the concatenated stealth patch source injected before any page script runs.
fn stealth_script() -> &'static str {
    STEALTH_SCRIPT
}

/// Inject the stealth patches into a freshly-created Page. Must be called
/// BEFORE any navigation on the page. Errors are silently ignored —
/// stealth is best-effort, a per-patch try/catch handles per-feature failures.
pub(crate) async fn apply_stealth_patches(page: &Page) {
    let params = AddScriptToEvaluateOnNewDocumentParams::new(stealth_script().to_string());
    if let Err(e) = page.execute(params).await {
        tracing::warn!(error = ?e, "stealth: failed to inject pre-document patches");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stealth_script_contains_all_six_patches() {
        let s = stealth_script();
        assert!(
            s.contains("Navigator.prototype, 'webdriver'"),
            "missing webdriver patch"
        );
        assert!(s.contains("'plugins'"), "missing plugins patch");
        assert!(s.contains("window.chrome.runtime"), "missing chrome.runtime patch");
        assert!(
            s.contains("UNMASKED_VENDOR_WEBGL") || s.contains("37445"),
            "missing WebGL patch"
        );
        assert!(
            s.contains("HTMLCanvasElement.prototype.toDataURL"),
            "missing canvas patch"
        );
        assert!(s.contains("navigator.permissions.query"), "missing permissions patch");
    }

    #[test]
    fn stealth_script_uses_try_catch_per_section() {
        let s = stealth_script();
        let try_count = s.matches("try {").count();
        assert!(try_count >= 6, "expected >=6 try/catch sections, got {}", try_count);
    }
}
