/* One voice turn: upload the staged recording, show what came back.
 *
 * Runs on its own task so a slow upload never stalls LVGL.
 */
#ifndef MAGESP_VOICE_H
#define MAGESP_VOICE_H

#include <stdbool.h>

typedef enum {
    TURN_IDLE,
    TURN_SENDING,     /* posting the WAV                    */
    TURN_THINKING,    /* posted, waiting on Magician        */
    TURN_DONE,        /* transcript and reply are available */
    TURN_SPEAKING,    /* playing the reply aloud             */
    TURN_ERROR,
} turn_state_t;

/* Create the upload worker. Call once at boot, while memory is plentiful. */
void voice_init(void);

/* Post one of the client-postable voice-note lifecycle events. Fire and
   forget: telemetry must never fail a turn. */
void voice_note_event(const char *event_type, const char *detail);

/* Submit whatever audio_stop() just finished writing. No-op if that
   recording is too short to be speech. */
void voice_submit(void);

turn_state_t voice_state(void);
const char  *voice_transcript(void);   /* what the user said, "" until known */
const char  *voice_reply(void);        /* Magician's answer, "" until known  */
const char  *voice_error(void);        /* why it failed, naming the cause    */

#endif /* MAGESP_VOICE_H */
