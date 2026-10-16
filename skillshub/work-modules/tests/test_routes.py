import ast
import json
import pathlib
import unittest

class SharedRoutesTests(unittest.TestCase):
    def test_self_contained_cli_and_host_have_the_same_reviewed_routes(self):
        root = pathlib.Path(__file__).parents[1]
        tree = ast.parse((root / 'bin/work-modules').read_text())
        node = next(x for x in tree.body if isinstance(x, ast.Assign) and any(getattr(t, 'id', None) == 'ROUTES' for t in x.targets))
        self.assertEqual(ast.literal_eval(node.value), json.loads((root / 'routes.json').read_text()))
