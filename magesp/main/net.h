/* Network bring-up and Magician reachability for the ESP32-C6 terminal.
 *
 * Credentials live in NVS, never in the build. With none stored the device
 * raises its own access point and serves a setup form; once stored it joins
 * the network and probes the configured Magician.
 */
#ifndef MAGESP_NET_H
#define MAGESP_NET_H

#include <stdbool.h>

typedef enum {
    NET_BOOTING,        /* nothing attempted yet                          */
    NET_PROVISIONING,   /* our AP is up, waiting for the setup form       */
    NET_CONNECTING,     /* joining the stored network                     */
    NET_NO_WIFI,        /* stored network would not accept us             */
    NET_NO_BACKEND,     /* on the network, Magician did not answer        */
    NET_ONLINE,         /* paired device identity/scope verified          */
} net_state_t;

void         net_start(void);
net_state_t  net_state(void);

/* One short line naming the thing the user needs: the AP to join, the SSID
   being tried, or the URL that did not answer. Never NULL. */
const char  *net_detail(void);

/* Dotted address this device is reachable at, or "" when it has none.
   Empty during provisioning, when 192.168.4.1 is the fixed AP address. */
const char  *net_ip(void);

/* Scope and destination the voice endpoints need. All return a usable value:
   defaults stand in until the setup form overrides them. */
const char  *net_base_url(void);
const char  *net_thread(void);
bool         net_configured(void);   /* a base URL has been set */

/* Cloudflare Access service token, empty when the URL is not behind Access. */
const char  *net_cf_id(void);
const char  *net_cf_secret(void);
bool         net_has_cf_token(void);

/* Claim the network while a turn runs, so the probe does not contend for
   the heap a TLS handshake needs. */
void         net_set_busy(bool busy);

/* ---- device identity ----
 * A stable id derived from the MAC, plus a Magician-issued pairing token so
 * the device is a named, revocable entry in the roster rather than an
 * anonymous caller.
 *
 * The token is sent as `Authorization: Bearer` and the server resolves both
 * principal and workspace from its paired-device record. */
const char  *net_device_id(void);
bool         net_has_device_token(void);
const char  *net_device_token(void);
void         net_pair_if_needed(void);

/* Dark theme. True black on AMOLED means unlit pixels, so this is a real
   power setting and not only a look. Persisted in NVS. */
bool         net_dark_theme(void);

/* ---- voice mode ----
 * Mirrors the server's AudioSurface taxonomy rather than inventing a parallel
 * one. Only DICTATE is implemented; the others are declared so the setting can
 * exist and report honestly what it cannot yet do. */
typedef enum {
    MODE_DICTATE = 0,   /* push to talk, one turn per press -- built        */
    MODE_HANDSFREE,     /* continuous mic, server VAD picks the turns       */
    MODE_REALTIME,      /* vendor realtime session over the same transport  */
} voice_mode_t;

voice_mode_t net_voice_mode(void);
void         net_set_voice_mode(voice_mode_t mode);
const char  *net_voice_mode_name(voice_mode_t mode);
bool         net_voice_mode_available(voice_mode_t mode);

#endif /* MAGESP_NET_H */
