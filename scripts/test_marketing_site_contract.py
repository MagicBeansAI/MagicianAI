#!/usr/bin/env python3
"""Static contracts for the standalone Cloudflare Pages marketing artifact."""

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
PUBLIC_DOCUMENT_ROUTES = ("privacy", "manifesto", "terms")


class MarketingSiteContractTests(unittest.TestCase):
    def test_public_document_routes_are_prerendered(self) -> None:
        for route in PUBLIC_DOCUMENT_ROUTES:
            route_config = (
                ROOT / "ui/unified-ui/src/routes" / route / "+page.ts"
            ).read_text()
            self.assertIn("export const prerender = true", route_config)

    def test_build_materializes_every_legacy_worker_name(self) -> None:
        makefile = (ROOT / "Makefile").read_text()
        source = "scripts/marketing-service-worker-retirement.js"
        for filename in ("service_worker.js", "service-worker.js", "sw.js"):
            self.assertIn(f"cp {source} marketing-site/{filename}", makefile)

    def test_tombstone_clears_cache_unregisters_and_reloads_clients(self) -> None:
        worker = (ROOT / "scripts/marketing-service-worker-retirement.js").read_text()
        self.assertIn("next.magican.ai", worker)
        self.assertIn("caches.keys()", worker)
        self.assertIn("self.registration.unregister()", worker)
        self.assertIn("client.navigate(client.url)", worker)

    def test_html_shell_does_not_repeat_the_retirement_migration(self) -> None:
        app_html = (ROOT / "ui/unified-ui/src/app.html").read_text()
        self.assertNotIn("navigator.serviceWorker.getRegistrations()", app_html)
        self.assertNotIn("%sveltekit.url%", app_html)
        self.assertIn("This shell is not the place for a", app_html)

    def test_pages_headers_do_not_cache_worker_tombstones(self) -> None:
        headers = (ROOT / "scripts/marketing-site-headers").read_text()
        for route in PUBLIC_DOCUMENT_ROUTES:
            self.assertIn(f"/{route}\n  Cache-Control: no-store", headers)
        for filename in ("service_worker.js", "service-worker.js", "sw.js"):
            self.assertIn(f"/{filename}\n  Cache-Control: no-store", headers)
        self.assertIn(
            "/_app/immutable/*\n  Cache-Control: public, max-age=31536000, immutable",
            headers,
        )

    def test_expired_cache_recovery_header_is_absent(self) -> None:
        headers = (ROOT / "scripts/marketing-site-headers").read_text()
        active_rules = "\n".join(
            line for line in headers.splitlines() if not line.lstrip().startswith("#")
        )
        self.assertNotIn("Clear-Site-Data:", active_rules)
        self.assertIn("one-release migration for the 0.0.774 recovery deploy", headers)

    def test_marketing_build_has_a_real_top_level_404(self) -> None:
        makefile = (ROOT / "Makefile").read_text()
        self.assertIn(
            "cp scripts/marketing-site-404.html marketing-site/404.html", makefile
        )
        not_found = (ROOT / "scripts/marketing-site-404.html").read_text()
        self.assertIn('<meta name="robots" content="noindex"', not_found)
        self.assertNotIn("%sveltekit", not_found)


if __name__ == "__main__":
    unittest.main()
