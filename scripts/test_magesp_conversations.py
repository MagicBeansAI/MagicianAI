"""Host checks of production conversation callbacks, without ESP hardware.

Compile the actual callback bodies with deterministic SDK doubles. In
particular, the worker may preempt the caller before voice_run_job returns.
"""
from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


def function_body(source, signature):
    start = source.index(signature)
    brace = source.index("{", start)
    depth = 0
    for end in range(brace, len(source)):
        depth += (source[end] == "{") - (source[end] == "}")
        if depth == 0:
            return source[start:end + 1]
    raise ValueError(f"unclosed function: {signature}")


PRELUDE = r'''
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "conv.h"
static conv_view_t s_view;
static char s_body[4096];
static int job_mode;
static int json_alive;
static const char *TAG = "test";
typedef struct { char *text; } cJSON;
static int request(const char *method, const char *path, char *out, size_t cap) {
    (void)method; (void)path; (void)cap; out[0] = 0; return 201;
}
static cJSON *cJSON_Parse(const char *text) {
    (void)text;
    cJSON *root = malloc(sizeof *root);
    root->text = strdup("new-thread");
    json_alive = 1;
    return root;
}
static cJSON *cJSON_GetObjectItem(cJSON *root, const char *key) { (void)key; return root; }
static const char *cJSON_GetStringValue(cJSON *item) { return item->text; }
static void cJSON_Delete(cJSON *root) { if (root) { json_alive = 0; free(root->text); free(root); } }
static void lock(void) {}
static void unlock(void) {}
static void persist(void) {}
static void changed(void) { s_view.version++; }
static void fail(const char *why) { (void)why; s_view.state = CONV_FAILED; changed(); }
static void copy(char *dst, size_t cap, const char *src) { snprintf(dst, cap, "%s", src); }
static void copy_ascii(char *dst, size_t cap, const char *src) { copy(dst, cap, src); }
static void do_refresh(void) { s_view.state = CONV_READY; changed(); }
static int net_configured(void) { return 1; }
static int net_has_device_token(void) { return 1; }
static bool voice_run_job(void (*job)(void)) {
    if (job_mode == 1) return false;
    if (job_mode == 2) { do_refresh(); return false; }
    job(); /* Simulate the worker preempting the UI as soon as it is woken. */
    return true;
}
static void checked_log(const char *fmt, const char *value) {
    assert(json_alive); /* Logging may not borrow a deleted JSON string. */
    printf(fmt, value);
}
#define ESP_LOGI(tag, fmt, value) checked_log(fmt, value)
'''

MAIN = r'''
int main(int argc, char **argv) {
    assert(argc == 2);
    if (strcmp(argv[1], "new") == 0) {
        do_new_thread();
        assert(strcmp(s_view.thread_id, "new-thread") == 0);
        assert(s_view.state == CONV_READY);
    } else if (strcmp(argv[1], "immediate") == 0) {
        assert(start(do_refresh));
        assert(s_view.state == CONV_READY);
    } else if (strcmp(argv[1], "busy") == 0) {
        job_mode = 1;
        assert(!start(do_refresh));
        assert(s_view.state == CONV_IDLE);
    } else if (strcmp(argv[1], "busy-completed") == 0) {
        job_mode = 2;
        assert(!start(do_refresh));
        assert(s_view.state == CONV_READY);
    } else { return 2; }
    return 0;
}
'''


class ConversationCallbacksTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        compiler = shutil.which("clang") or shutil.which("cc")
        if not compiler:
            raise RuntimeError("C compiler required for conversation callback tests")
        cls.directory = tempfile.TemporaryDirectory(prefix="magesp-conversations-")
        cls.addClassCleanup(cls.directory.cleanup)
        directory = Path(cls.directory.name)
        source = Path(os.environ.get("MAGESP_CONV_TEST_SOURCE", ROOT / "magesp/main/conv.c")).read_text()
        unit = directory / "callbacks.c"
        unit.write_text(PRELUDE + "\n" + function_body(source, "static void do_new_thread(void)")
                        + "\n" + function_body(source, "static bool start(void (*job)(void))") + MAIN)
        cls.binary = directory / "callbacks"
        build = subprocess.run([compiler, "-g",
                                "-I", str(ROOT / "magesp/main"), str(unit), "-o", str(cls.binary)],
                               capture_output=True, text=True, timeout=30)
        if build.returncode:
            raise RuntimeError(build.stdout + build.stderr)

    def run_callback(self, case):
        result = subprocess.run([str(self.binary), case], capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_new_thread_does_not_log_freed_json(self):
        self.run_callback("new")

    def test_a_fast_worker_completion_stays_ready(self):
        self.run_callback("immediate")

    def test_a_busy_worker_keeps_the_prior_state(self):
        self.run_callback("busy")

    def test_a_rejected_job_does_not_overwrite_another_completion(self):
        self.run_callback("busy-completed")


if __name__ == "__main__":
    unittest.main()
