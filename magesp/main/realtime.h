/* Realtime voice over Magician's BackendProxied profile.
 *
 * Separate from voice.c on purpose. The turn-based loop in voice.c works, and
 * it is what the device does for a living, so realtime is built alongside it
 * rather than through it: nothing in the turn path calls into this file, and
 * nothing here runs unless the owner selects realtime and starts a call.
 *
 * Design: docs/plans/2026-08-14-esp32-realtime-proxied-design.md
 */
#ifndef MAGESP_REALTIME_H
#define MAGESP_REALTIME_H

#include <stdbool.h>
#include <stdint.h>

typedef enum {
    RT_OFF = 0,
    RT_OPENING,     /* registering the session, connecting the socket */
    RT_LIVE,        /* session.ready seen; the call is up             */
    RT_SPEAKING,    /* assistant audio is arriving and playing        */
    RT_FAILED,      /* gave up; realtime_error() says why             */
} rt_state_t;

/* Start or stop a call. Starting registers a media session, opens the control
 * WS and sends session.start; stopping sends session.end and releases
 * everything. Both return immediately -- the call runs on its own task. */
bool realtime_start(void);
void realtime_stop(void);

rt_state_t  realtime_state(void);
const char *realtime_state_name(void);
const char *realtime_error(void);   /* "" unless RT_FAILED            */
int         realtime_seconds(void); /* elapsed call time, 0 when off  */
uint32_t    realtime_uplink_bytes(void);
uint32_t    realtime_uplink_drops(void);
uint32_t    realtime_downlink_bytes(void);
uint32_t    realtime_responses(void);

/* Heap probe with the socket open. Kept because a realtime call is the one
 * thing on this device that holds a TLS session for minutes, and every failure
 * on this board so far has been heap exhaustion wearing a different mask. */
bool realtime_spike(int hold_secs);

#endif /* MAGESP_REALTIME_H */
