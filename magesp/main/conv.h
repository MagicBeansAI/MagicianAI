#ifndef MAGESP_CONV_H
#define MAGESP_CONV_H

#include <stdbool.h>

/* Which Magician thread and chat this terminal talks in, chosen on its own
 * screen (the Conversations page) and kept in NVS across restarts.
 *
 * Until something is picked, turns go to the server's default for this
 * device: a thread of its own, named after it. "New thread" starts a thread
 * named after the device and the time, filed under Automated. */

#define CONV_MAX_THREADS  8
#define CONV_MAX_SESSIONS 6
#define CONV_ID_LEN       72
#define CONV_NAME_LEN     48

typedef enum { CONV_IDLE, CONV_LOADING, CONV_READY, CONV_FAILED } conv_state_t;

typedef struct {
    char id[CONV_ID_LEN];
    char name[CONV_NAME_LEN];
} conv_item_t;

/* Everything the screen shows, copied out in one go. */
typedef struct {
    unsigned     version;               /* changes whenever anything below does */
    conv_state_t state;
    char         error[64];
    bool         picked;                /* a thread was chosen on this screen   */
    char         thread_id[CONV_ID_LEN];
    char         thread_name[CONV_NAME_LEN];
    char         session_id[CONV_ID_LEN];   /* "" = the thread's latest chat   */
    char         session_title[CONV_NAME_LEN];
    int          n_threads, n_sessions;
    conv_item_t  threads[CONV_MAX_THREADS];
    conv_item_t  sessions[CONV_MAX_SESSIONS];
} conv_view_t;

void     conv_init(void);
unsigned conv_version(void);
void     conv_snapshot(conv_view_t *out);

/* Network actions run on the voice turn worker, so they never overlap a turn
   or each other; each returns false when a turn or another action is busy. */
bool conv_refresh(void);            /* lists for the current thread           */
bool conv_new_thread(void);         /* start a thread, select it, refresh     */

void conv_select_thread(int index); /* picks it (latest chat) and refreshes    */
void conv_select_session(int index);/* -1 = the thread's latest chat          */

/* For a voice note: the thread to send, whether it was picked on this
   screen, and the chosen session id ("" when none). */
const char *conv_turn_thread(bool *picked);
const char *conv_turn_session(void);

#endif /* MAGESP_CONV_H */
