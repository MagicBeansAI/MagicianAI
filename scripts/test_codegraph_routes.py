#!/usr/bin/env python3
"""Actix endpoint discovery must survive rustfmt's multiline trailing commas."""
import importlib.util
from pathlib import Path
import unittest

SCRIPT_DIR = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "generate_code_graph", SCRIPT_DIR / "generate_code_graph.py"
)
codegraph = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(codegraph)


class ActixRouteTests(unittest.TestCase):
    def test_rustfmt_preserves_discovered_routes(self):
        compact = 'web::scope("/api").route("/settings", web::put().to(update_settings))'
        formatted = '''web::scope("/api")
            .route(
                "/settings",
                web::put().to(update_settings),
            )'''
        for source in (compact, formatted):
            with self.subTest(source=source):
                endpoints = codegraph.extract_actix_endpoints(source)
                self.assertEqual(len(endpoints), 1)
                self.assertEqual(endpoints[0]["method"], "PUT")
                self.assertEqual(endpoints[0]["path"], "/api/settings")
                self.assertEqual(endpoints[0]["handler"], "update_settings")

    def test_formatted_resource_route_with_qualified_handler(self):
        source = '''web::resource("/settings")
            .route(
                web::get().to(api::settings_get),
            )'''
        endpoints = codegraph.extract_actix_endpoints(source)
        self.assertEqual(len(endpoints), 1)
        self.assertEqual(endpoints[0]["path"], "/settings")
        self.assertEqual(endpoints[0]["handler"], "api::settings_get")


if __name__ == "__main__":
    unittest.main()
