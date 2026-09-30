"""Provider-free tests of the voice-delegation conformance lane's contracts."""
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

if __package__:
    from . import eval_harness_voice as voice
else:
    import eval_harness_voice as voice


WORDS = ("amber", "cedar", "plum")


def frame(kind, **payload):
    return {"kind": kind, "payload": payload}


READY = frame("session.ready", voice_session_id="vs-1", tools=[{"name": "delegate_to_chat"}, {"name": "list_tasks"}],
              descriptor={"model": "gpt-realtime-2.1", "provider": "open_ai", "turn_detection_mode": "none"})
DELEGATE_RESULT = frame("tool.result", call_id="call_1", tool_name="delegate_to_chat", status="streaming",
                        output=json.dumps({"tool_name": "delegate_to_chat", "status": "streaming"}))
DONE = frame("delegate_to_chat.done", call_id="call_1", chunk_count=1, success=True, error=None)


def turn_event(kind, tool_name=None, chat_turn_id="voice-turn-1"):
    payload = {"chat_turn_id": chat_turn_id}
    if tool_name:
        payload["tool_name"] = tool_name
    return {"event_type": "transport", "data": {"event": {"event_type": kind, "payload": payload}}}


class NonceContracts(unittest.TestCase):
    def test_the_title_is_spoken_words_and_matched_loosely(self):
        title = voice.probe_title(WORDS)
        self.assertEqual(title, "Voice probe amber cedar plum")
        # Speech-to-text may change case and punctuation; every word must be there.
        self.assertTrue(voice.title_matches("voice probe Amber, cedar plum.", WORDS))
        self.assertTrue(voice.title_matches("Voice-probe-amber-cedar-plum", WORDS))
        self.assertTrue(voice.title_matches("VoiceProbe-Amber-Cedar-Plum", WORDS))
        self.assertTrue(voice.title_matches("VoiceProbe-AmberCedarPlum", WORDS), "camel-cased words are words")
        # A transcriber that drops the end of "probe" still named the task.
        self.assertTrue(voice.title_matches("voice pro amber cedar plum", WORDS))
        self.assertEqual(voice.heard_words("titled 'voice pro amber cedar plum' with", 3), ["amber", "cedar", "plum"])
        self.assertFalse(voice.title_matches("Voice probe amber cedar", WORDS))
        self.assertFalse(voice.title_matches("Voice probe amber cedar plum", ("amber", "cedar", "fig")))
        # A delegation brief that quotes the title is not the task that was asked for.
        self.assertFalse(voice.title_matches(
            'executive-assistant / Create a tracked task titled exactly "voice-probe amber cedar plum"', WORDS))
        # The transcript is matched on the words alone.
        self.assertTrue(voice.transcript_heard("ask my jacket to create a task titled voice probe amber, cedar plum with", WORDS))
        self.assertFalse(voice.transcript_heard("voice probe or kid amber plum", WORDS))

    def test_a_nonce_is_three_distinct_words_from_the_spoken_list(self):
        words = voice.word_nonce()
        self.assertEqual(len(words), 3)
        self.assertEqual(len(set(words)), 3)
        self.assertTrue(all(word in voice.SPOKEN_WORDS for word in words))

    def test_the_prompt_is_the_request_itself_with_no_name_for_a_transcriber_to_mangle(self):
        # "Ask Magician" came back from the local transcriber as "ask my jacket",
        # and the delegated turn went looking for a specialist called Jacket.
        # The request needs no name: the realtime catalog has no task-creating
        # hand, so creating a task is itself the reason to delegate — or to
        # load the hand and do it, which the lane counts as self-served.
        prompt = voice.voice_prompt(WORDS, "voice_realtime_openai_backend")
        self.assertEqual(prompt, voice.voice_prompt(WORDS, "voice_realtime_gpt_live_1"))
        self.assertTrue(prompt.startswith("Create a task titled Voice probe amber cedar plum"))
        self.assertNotIn("Magician", prompt)
        self.assertIn("voice conformance probe", prompt)

    def test_the_words_as_heard_are_read_off_the_transcript(self):
        # The heard title runs from the prefix to the request's next phrase,
        # however many tokens the transcriber made of it: four for three words
        # split ("guard in"), two for three words merged ("tattlehammer").
        self.assertEqual(voice.heard_words("Create a task titled voice probe guard in orchid amber with the", 3), ["guard", "in", "orchid", "amber"])
        self.assertEqual(voice.heard_words('titled "Voice Probe Plum Violet Summit" with', 3), ["plum", "violet", "summit"])
        self.assertEqual(voice.heard_words("voice probe tattlehammer button with the description", 3), ["tattlehammer", "button"])
        self.assertEqual(voice.heard_words("voice probe amber cedar plum", 3), ["amber", "cedar", "plum"])
        self.assertIsNone(voice.heard_words("see voice number eight", 3))

    def test_a_task_titled_as_heard_counts_even_when_the_transcriber_merged_words(self):
        # "turtle hammer button" came back as "tattlehammer button": the task
        # carries the heard title, two tokens for three words.
        heard = "create a task titled voice probe tattlehammer button with the description"
        self.assertTrue(voice.title_as_heard("voice-probe.tattlehammer.button", heard, 3))
        self.assertFalse(voice.title_as_heard("voice probe tattlehammer", heard, 3), "part of the heard title is not the title")
        self.assertFalse(voice.title_as_heard("voice probe rabbit", "voice probe rabbit jungle tiger", 3))
        self.assertTrue(voice.title_as_heard("Voice probe guard in orchid amber", "titled voice probe guard in orchid amber with the", 3))

    def test_every_user_transcript_of_the_call_is_read_not_only_the_last(self):
        # A server VAD can split one request into two user turns; the title
        # was in the first, the tail in the second.
        folded = voice.fold_frames([READY, frame("transcript.user", text="create a task titled voice probe kitten dragon silver with the"),
                                    DELEGATE_RESULT, frame("transcript.user", text="conformance probe and tell me when it is created"), DONE])
        self.assertEqual(folded["user_transcript"], "conformance probe and tell me when it is created")
        self.assertEqual(folded["user_transcripts"], ["create a task titled voice probe kitten dragon silver with the",
                                                      "conformance probe and tell me when it is created"])
        gates = voice.grade_run(folded=folded, driver="speech", words=("kitten", "dragon", "silver"), voice_rows=[],
                                turn_events=[], task={"title": "Voice probe kitten dragon silver"})
        self.assertTrue(gates["effect"])


class DriverContracts(unittest.TestCase):
    def test_the_roster_is_every_selectable_assistant_profile_and_gemini_speaks_only(self):
        self.assertEqual(voice.PROFILES, ("voice_realtime_openai_backend", "voice_realtime_gpt_live_1",
                                          "voice_realtime_gemini_38_live", "voice_realtime_gemini_38_live_thinking"))
        self.assertEqual(voice.driver_for("voice_realtime_gemini_38_live", "auto"), "speech")
        with self.assertRaises(ValueError):
            voice.driver_for("voice_realtime_gemini_38_live_thinking", "text")

    def test_speech_is_the_default_driver_and_only_realtime_can_be_typed_at(self):
        # A typed prompt reaches GPT Realtime as a system item — the model may
        # decline to act on it — and reaches GPT Live 1 only as commentary. A
        # spoken prompt is a user turn on both, so speech is the default.
        self.assertEqual(voice.driver_for("voice_realtime_openai_backend", "auto"), "speech")
        self.assertEqual(voice.driver_for("voice_realtime_gpt_live_1", "auto"), "speech")
        self.assertEqual(voice.driver_for("voice_realtime_openai_backend", "text"), "text")
        with self.assertRaises(ValueError):
            voice.driver_for("voice_realtime_gpt_live_1", "text")

    def test_pcm_is_framed_at_the_realtime_frame_size_with_trailing_silence(self):
        pcm = b"\x01\x02" * 12_000  # 0.5 s at 24 kHz mono s16le
        frames = voice.pcm_frames(pcm, silence_secs=1.0)
        self.assertTrue(all(len(f) <= voice.REALTIME_FRAME_BYTES for f in frames))
        self.assertEqual(sum(len(f) for f in frames), len(pcm) + 24_000 * 2)
        self.assertEqual(frames[-1], b"\x00" * len(frames[-1]))
        self.assertEqual(b"".join(frames[:3])[:len(pcm)][:6], b"\x01\x02\x01\x02\x01\x02")

    def test_the_turn_boundary_follows_the_descriptor(self):
        self.assertTrue(voice.uses_push_to_talk({"turn_detection_mode": "none"}))
        self.assertFalse(voice.uses_push_to_talk({"turn_detection_mode": "server_vad"}))


class FoldContracts(unittest.TestCase):
    def test_the_delegation_is_read_off_the_tool_result_and_done_frames(self):
        folded = voice.fold_frames([READY, frame("transcript.assistant", text="On it."), DELEGATE_RESULT,
                                    frame("delegate_to_chat.chunk", call_id="call_1", sequence=0, text="Created."),
                                    DONE, frame("transcript.user", text="voice probe amber cedar plum")])
        self.assertEqual(folded["voice_session_id"], "vs-1")
        self.assertEqual(folded["tools"], ["delegate_to_chat", "list_tasks"])
        self.assertEqual(folded["delegate_call_ids"], ["call_1"])
        self.assertEqual(folded["done"], DONE["payload"])
        self.assertEqual(folded["assistant_text"], "On it.")
        self.assertEqual(folded["user_transcript"], "voice probe amber cedar plum")
        self.assertEqual(folded["chunks"], ["Created."])
        self.assertIsNone(folded["error"])

    def test_a_session_error_and_an_absent_delegation_are_visible(self):
        folded = voice.fold_frames([READY, frame("session.error", message="provider gone", recoverable=False)])
        self.assertEqual(folded["error"], "provider gone")
        self.assertEqual(folded["delegate_call_ids"], [])
        self.assertIsNone(folded["done"])


class Grading(unittest.TestCase):
    def evidence(self, **overrides):
        base = {
            "folded": voice.fold_frames([READY, DELEGATE_RESULT, DONE, frame("transcript.user", text="voice probe amber cedar plum")]),
            "driver": "speech",
            "words": WORDS,
            "voice_rows": [{"capability": "voice.realtime", "chat_session_id": "cs-1", "chat_turn_id": "voice-turn-1",
                            "provider": "openai", "model": "gpt-realtime-2.1", "success": True,
                            "cost_usd": 0.0123, "cost_source": "provider"}],
            "turn_events": [turn_event("tool.call.started", "tool_search"), turn_event("tool.call.started", "create_task"),
                            turn_event("tool.result.projected", "create_task")],
            "task": {"task_id": "task-1", "title": "Voice probe amber cedar plum"},
        }
        base.update(overrides)
        return base

    def test_a_delegated_run_that_created_the_task_has_every_proof(self):
        proof = voice.grade_run(**self.evidence())
        self.assertTrue(all(proof.values()), proof)

    def test_the_effect_is_the_task_and_the_hands_are_on_the_voice_turn(self):
        self.assertFalse(voice.grade_run(**self.evidence(task=None))["effect"])
        self.assertFalse(voice.grade_run(**self.evidence(task={"title": "Voice probe amber cedar"}))["effect"])
        # A task titled as the transcriber heard the request is the task that
        # was asked for, as far as Magician could know; the transcriber's slip
        # is reported beside it, not charged to the delegation.
        misheard = voice.fold_frames([READY, DELEGATE_RESULT, DONE, frame("transcript.user", text="create a task titled voice probe amber seed plum with")])
        self.assertTrue(voice.grade_run(**self.evidence(folded=misheard, task={"title": "Voice probe amber seed plum"}))["effect"])
        self.assertFalse(voice.grade_run(**self.evidence(folded=misheard, task={"title": "Voice probe amber cedar fig"}))["effect"])
        no_hands = voice.grade_run(**self.evidence(turn_events=[turn_event("tool.call.started", "tool_search")]))
        self.assertFalse(no_hands["hands_on_turn"])
        self.assertFalse(voice.grade_run(**self.evidence(voice_rows=[]))["voice_call_row"])
        self.assertFalse(voice.grade_run(**self.evidence(voice_rows=[{"capability": "chat", "success": True}]))["voice_call_row"])

    def test_a_realtime_call_is_accounted_only_when_it_succeeded_and_priced_only_when_it_costs(self):
        """A failed row is not an accounted call, and a duration-billed mouth
        with no price at all was not metered — GPT-Live is charged by the
        clock, so an unpriced Live session is a bill nobody recorded."""
        failed = [{"capability": "voice.realtime", "chat_turn_id": "t1", "success": False}]
        self.assertFalse(voice.grade_run(**self.evidence(voice_rows=failed))["voice_call_row"])
        unpriced = [{"capability": "voice.realtime", "chat_turn_id": "t1", "success": True, "cost_usd": None}]
        gates = voice.grade_run(**self.evidence(voice_rows=unpriced))
        self.assertTrue(gates["voice_call_row"])
        self.assertFalse(gates["voice_call_priced"])
        priced = [{"capability": "voice.realtime", "chat_turn_id": "t1", "success": True, "cost_usd": 0.01}]
        self.assertTrue(voice.grade_run(**self.evidence(voice_rows=priced))["voice_call_priced"])

    def test_the_delegated_turn_is_the_voice_turn_the_row_names_or_the_delegate_call(self):
        # GPT Realtime runs the delegation under the voice turn its call row
        # names; GPT Live 1 runs it under the delegation id, which is also the
        # delegate call id, and writes no attributed row. Both are read, and
        # the session is the voice channel session on the probe's UI thread.
        folded = voice.fold_frames([READY, DELEGATE_RESULT, DONE])
        rows = [{"capability": "voice.realtime", "chat_turn_id": "voice-turn-1"}, {"capability": "voice.realtime", "chat_turn_id": "voice-turn-1"}]
        self.assertEqual(voice.delegated_turn_ids(folded, rows), ["voice-turn-1", "call_1"])
        self.assertEqual(voice.delegated_turn_ids(folded, [{"chat_turn_id": None}]), ["call_1"])
        self.assertEqual(voice.delegated_turn_ids(voice.fold_frames([READY]), []), [])

    def test_what_the_transcriber_heard_is_reported_but_does_not_decide(self):
        heard_wrong = voice.fold_frames([READY, DELEGATE_RESULT, DONE, frame("transcript.user", text="see voice number eight")])
        story = voice.describe_run(folded=heard_wrong, words=WORDS, task={"title": "Voice probe amber cedar plum"},
                                   turn_events=self.evidence()["turn_events"], proof=voice.grade_run(**self.evidence(folded=heard_wrong)))
        self.assertEqual(story["outcome"], "reached")
        self.assertFalse(voice.transcript_heard(heard_wrong["user_transcript"], WORDS))


def tool_result(name, status="ok", message=None):
    output = {"tool_name": name, "status": status}
    if message:
        output["result"] = {"message": message}
    return frame("tool.result", call_id=f"call_{name}", status=status, output=json.dumps(output))


class Story(unittest.TestCase):
    """Each run is told as what was asked, what the mouth did, and whether the
    person got it. The proof gates stay underneath as evidence."""

    def story(self, frames, task, turn_events=(), voice_rows=None):
        folded = voice.fold_frames(frames)
        proof = voice.grade_run(folded=folded, driver="speech", words=WORDS,
                                voice_rows=voice_rows if voice_rows is not None else [{"capability": "voice.realtime", "chat_turn_id": "t"}],
                                turn_events=list(turn_events), task=task)
        return voice.describe_run(folded=folded, words=WORDS, task=task, turn_events=list(turn_events), proof=proof)

    def test_a_delegation_that_created_the_task_reached_the_result(self):
        story = self.story([READY, DELEGATE_RESULT, frame("delegate_to_chat.chunk", call_id="call_1", sequence=0, text="Created."), DONE],
                           {"title": "Voice probe amber cedar plum"},
                           [turn_event("tool.call.started", "tool_search"), turn_event("tool.call.started", "create_task")])
        self.assertEqual(story["expected"], "a task titled \"Voice probe amber cedar plum\"")
        self.assertEqual(story["path"], "delegated")
        self.assertIn("delegate_to_chat", story["did"])
        self.assertIn("tool_search, create_task", story["did"])
        self.assertIn("Created.", story["did"])
        self.assertEqual(story["outcome"], "reached")
        self.assertIn("Voice probe amber cedar plum", story["result"])

    def test_a_mouth_that_loaded_the_hand_and_did_it_itself_reached_the_result(self):
        story = self.story([READY, tool_result("tool_search"), tool_result("create_task"),
                            frame("transcript.assistant", text="It's created.")], {"title": "voice-probe-amber-cedar-plum"})
        self.assertEqual(story["path"], "self_served")
        self.assertIn("tool_search, create_task", story["did"])
        self.assertEqual(story["outcome"], "reached")

    def test_a_mouth_that_declined_did_not_reach_it_and_its_words_are_kept(self):
        story = self.story([READY, frame("transcript.assistant", text="I don't have the ability to create tasks.")], None)
        self.assertEqual(story["path"], "declined")
        self.assertIn("I don't have the ability", story["did"])
        self.assertEqual(story["outcome"], "not_reached")
        self.assertIn("no task", story["result"])

    def test_a_delegation_whose_brain_gave_up_is_told_in_the_brain_s_words(self):
        story = self.story([READY, DELEGATE_RESULT, frame("delegate_to_chat.chunk", call_id="call_1", sequence=0,
                                                          text="I can create it, but task creation isn't available in my current tool set."), DONE], None)
        self.assertEqual(story["path"], "delegated")
        self.assertEqual(story["outcome"], "not_reached")
        self.assertIn("isn't available", story["did"])

    def test_a_refused_hand_or_a_session_error_is_an_error_path(self):
        refused = self.story([READY, tool_result("tool_search"), tool_result("create_task", "error", "external tool policy snapshot changed"),
                              frame("transcript.assistant", text="The tool failed.")], None)
        self.assertEqual(refused["path"], "errored")
        self.assertIn("policy snapshot changed", refused["did"])
        broken = self.story([READY, frame("session.error", message="provider gone", recoverable=False)], None)
        self.assertEqual(broken["path"], "errored")
        self.assertIn("provider gone", broken["did"])

    def test_a_delegation_still_waiting_when_the_call_ended_says_so(self):
        story = self.story([READY, DELEGATE_RESULT], None, [turn_event("tool.call.started", "need_user_input")])
        self.assertEqual(story["path"], "delegated")
        self.assertIn("never finished", story["did"])
        self.assertIn("need_user_input", story["did"])
        self.assertEqual(story["outcome"], "not_reached")


class SummaryContracts(unittest.TestCase):
    def test_the_report_counts_outcomes_and_paths_per_profile(self):
        results = [
            {"engine": "p", "outcome": "reached", "path": "delegated"},
            {"engine": "p", "outcome": "reached", "path": "self_served"},
            {"engine": "p", "outcome": "not_reached", "path": "declined"},
            {"engine": "p", "outcome": "not_reached", "path": "delegated"},
            {"engine": "q", "outcome": "inconclusive", "path": None},
        ]
        summary = voice.summarize(results)
        self.assertEqual((summary["p"]["runs"], summary["p"]["reached"]), (4, 2))
        self.assertEqual(summary["p"]["paths"], {"delegated": {"runs": 2, "reached": 1}, "self_served": {"runs": 1, "reached": 1},
                                                 "declined": {"runs": 1, "reached": 0}})
        self.assertEqual(summary["q"]["inconclusive"], 1)
        with tempfile.TemporaryDirectory() as directory:
            payload = voice.write_report(Path(directory), results, "live")
            self.assertEqual(payload["lane"], "voice")
            self.assertFalse(payload["summary"]["ok"])
            self.assertEqual(payload["summary"]["reached"], 2)
            self.assertTrue((Path(directory) / "report.html").exists())
        with tempfile.TemporaryDirectory() as directory:
            self.assertTrue(voice.write_report(Path(directory), results[:2], "live")["summary"]["ok"])


class RunnerContracts(unittest.TestCase):
    def test_an_unreachable_runtime_is_inconclusive_for_every_planned_run(self):
        class Unavailable(RuntimeError):
            pass

        client = Mock()
        client.json.side_effect = Unavailable("connection refused")
        with tempfile.TemporaryDirectory() as directory:
            args = SimpleNamespace(self_test=False, api_base_url="http://localhost:1", engines=None, runs=2,
                                   voice_driver="auto", turn_timeout_secs=30, http_timeout_secs=5,
                                   output_dir=Path(directory), keep_artifacts=False)
            with patch.object(voice, "login", side_effect=Unavailable("service offline")):
                code = voice.main(args, lambda *a, **k: client, Unavailable)
            self.assertEqual(code, 2)
            report = json.loads((Path(directory) / "report.json").read_text())
            self.assertEqual(len(report["results"]), 2 * len(voice.PROFILES))
            self.assertTrue(all(row["outcome"] == "inconclusive" for row in report["results"]))


if __name__ == "__main__":
    unittest.main()
