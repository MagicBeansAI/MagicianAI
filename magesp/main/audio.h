/* Microphone capture for the ESP32-C6 terminal.
 *
 * Records 16 kHz mono PCM16 to a WAV file on flash while push-to-talk is
 * held. Flash rather than RAM because this chip has no PSRAM and a usable
 * utterance is larger than the free heap; staging to a file also keeps a
 * correct WAV header and a real Content-Length for the upload, which a
 * streamed body could not state.
 */
#ifndef MAGESP_AUDIO_H
#define MAGESP_AUDIO_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* Mount storage, bring up I2S and the ES8311. Safe to call once at boot. */
bool audio_init(void);

/* True when the codec came up. Everything below is inert when false. */
bool audio_ready(void);

void audio_start(void);
void audio_stop(void);

/* True only once the codec has actually delivered a buffer.
 *
 * The screen must be driven by THIS, not by the button: reporting "listening"
 * because a finger moved is the indicator lie this project keeps hitting. */
bool audio_live(void);

/* ---- hands free ----
 * The same capture path, with the device deciding where the turn starts and
 * ends instead of a finger. Endpointing is local, on the samples: streaming
 * raw audio to a server VAD would cost a second permanent TLS session on a
 * part with ~130 KB of free heap, to learn something a peak meter already
 * knows here. The transport, upload and reply path are unchanged. */
bool audio_arm(void);            /* open the mic; keep nothing until speech  */
void audio_open_gate(void);      /* a button press outranks the gate         */
void audio_disarm(void);         /* stop an armed capture that never spoke   */
bool audio_capturing(void);      /* the mic task is running, gated or not    */
bool audio_speech_started(void); /* the gate opened; audio is being kept     */
bool audio_utterance_done(void); /* the endpointer called the turn finished  */
bool audio_speaker_busy(void);   /* the reply is playing; the mic must stay shut */

/* Drop the audio hardware once nothing has needed it for a few seconds. Call
   from the UI tick. Never tears down between the sentences of one reply. */
void audio_idle_tick(void);

/* Keep the hardware up across a whole reply. A reply is spoken one sentence
   per TTS request, so the gap between sentences is a network round trip -- far
   longer than any idle grace. The turn says when it is done. */
void audio_hold(bool on);

/* ---- streaming capture, for realtime ----
 * A separate path from the turn loop on purpose. The turn loop stages to a
 * file, endpoints locally and uploads once; realtime wants raw chunks as they
 * arrive and lets the server decide where turns begin and end. Sharing one
 * capture task between those two would put a mode flag through every branch of
 * the thing this project has spent the longest getting right.
 *
 * Refuses while a turn recording is running, and vice versa: one microphone. */
typedef void (*audio_chunk_cb)(const void *pcm, int len);

bool audio_stream_start(int rate, audio_chunk_cb cb);
void audio_stream_stop(void);
bool audio_streaming(void);

/* Ask for shallow I2S DMA on the next bring-up. Realtime needs it: its socket
   has already taken the heap the deep buffers would want. */
void audio_set_lean(bool lean);

size_t audio_bytes(void);       /* PCM bytes in the last/current recording */
int    audio_peak(void);        /* peak |sample| 0..32767, so silence is detectable */
const char *audio_path(void);   /* WAV file path on the mounted filesystem  */

/* ---- playback ----
 * Magician returns 24 kHz mono PCM16, which the ES8311 takes directly: no
 * decoder, which matters on a part with no PSRAM. Open, write the stream as
 * it arrives, close. */
bool audio_speaker_open(int sample_rate);
int  audio_speaker_write(const void *data, int len);
void audio_speaker_close(void);

#endif /* MAGESP_AUDIO_H */
