/* Minimal Vosk C API surface — HAND-WRITTEN, and deliberately not the upstream
 * header.
 *
 * The iOS `libvosk.a` in circulation is an Alpha Cephei build from 2021 whose
 * exported symbol table is a strict SUBSET of the one the upstream header
 * declares. Vendoring upstream's header would therefore let this app compile
 * against functions that are not in the binary, and the failure would surface as
 * an undefined-symbol LINK error — or, worse, as a runtime trap in whichever
 * build first exercised the path.
 *
 * The important instance is `vosk_recognizer_set_grm`, which exists on Android
 * and is ABSENT here. Grammar changes therefore have to destroy and rebuild the
 * recognizer, and the only thing that reliably stops someone reaching for the
 * cheaper-looking call is its not being declared anywhere they can see it. That
 * is the whole reason this file is a subset rather than a copy: every function
 * below was read out of the shipped archive with `nm`, so if it compiles, it
 * links.
 *
 * Verified present in both slices of libvosk.xcframework (v2.1.7 vendoring of
 * the Alpha Cephei build) via `nm -g`. Adding a declaration here without
 * checking `nm` first defeats the point of the file.
 */

#ifndef MAGIOS_VOSK_API_H
#define MAGIOS_VOSK_API_H

#ifdef __cplusplus
extern "C" {
#endif

typedef struct VoskModel VoskModel;
typedef struct VoskRecognizer VoskRecognizer;

/** Load an unpacked model directory. Returns NULL on failure. */
VoskModel *vosk_model_new(const char *model_path);

/** Release a model. Safe on NULL. */
void vosk_model_free(VoskModel *model);

/** Word id in the model's lexicon, or -1 if the model has never heard of it.
 *
 *  This is the ONLY way to tell whether a phrase can actually be recognised:
 *  words missing from the lexicon are dropped from a grammar with nothing but a
 *  log line, so the phrase silently shortens instead of failing. */
int vosk_model_find_word(VoskModel *model, const char *word);

/** Create a recognizer constrained to a JSON array of phrases.
 *
 *  The array MUST include "[unk]" — see `VoskWakeSpotter`. There is no
 *  set-grammar counterpart in this binary; re-grammaring means a new recognizer. */
VoskRecognizer *vosk_recognizer_new_grm(VoskModel *model, float sample_rate, const char *grammar);

/** Feed signed 16-bit mono PCM. `length` is a COUNT OF SAMPLES, not bytes.
 *  Returns 1 when an utterance completed, 0 while one is still in progress. */
int vosk_recognizer_accept_waveform_s(VoskRecognizer *recognizer, const short *data, int length);

/** JSON for the utterance that just completed. Owned by the recognizer. */
const char *vosk_recognizer_result(VoskRecognizer *recognizer);

/** JSON for the utterance in progress. Owned by the recognizer.
 *
 *  NOT used by `VoskWakeSpotter`, which fires only on finalised results — see
 *  its `feed`. Declared because the accuracy-measurement suite compares the two
 *  modes, and that comparison has to stay reproducible for Task 13's device
 *  re-measurement. It is not an alternative for the shipping hit path. */
const char *vosk_recognizer_partial_result(VoskRecognizer *recognizer);

/** Drop all decoder state, including any partial utterance. */
void vosk_recognizer_reset(VoskRecognizer *recognizer);

/** Release a recognizer. Safe on NULL. */
void vosk_recognizer_free(VoskRecognizer *recognizer);

/** Global log verbosity. Negative silences Kaldi's chatter. */
void vosk_set_log_level(int log_level);

#ifdef __cplusplus
}
#endif

#endif /* MAGIOS_VOSK_API_H */
