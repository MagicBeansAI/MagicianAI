/* Tilt maze for the ESP32-C6 AMOLED terminal.
 *
 * Idle draws a generated labyrinth you steer by tilting the board; holding
 * the BOOT button puts the game away and shows the voice orb.
 *
 * The maze is a perfect maze (exactly one route between any two cells) built
 * by randomised depth-first search at boot, so every power-up is a new one
 * and dead ends are real. Physics runs inside an LVGL timer, sharing the
 * LVGL task, so it needs no extra locking.
 */
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "bsp/esp-bsp.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "esp_log.h"
#include "esp_random.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "lvgl.h"
#include "audio.h"
#include "realtime.h"
#include "voice.h"
#include "net.h"
#include "qmi8658.h"

static const char *TAG = "maze";

#define SCR_W        BSP_LCD_H_RES
#define SCR_H        BSP_LCD_V_RES

/* ---- ring geometry ----
 * Concentric rings, each with exactly one gap at a random angle. Every ring
 * is a search: you have to run the circumference to find the opening, and
 * momentum keeps carrying you past it. Far harder than a grid maze at this
 * screen size, where wide corridors leave too few decisions per run. */
/* The panel has physically ROUNDED CORNERS: anything drawn into a screen
 * corner is hidden by the glass. So the playfield is a circle, not the
 * screen rectangle -- the ball is bounced off an arena boundary well inside
 * the corner radius and can never reach a place it cannot be seen. */
#define ARENA_R      176.0f
#define RINGS        4
#define RING_T       4
#define GAP_ARC_PX   40.0f             /* opening measured in pixels, not
                                          degrees, so outer and inner rings
                                          are equally passable */
#define CX           (SCR_W / 2.0f)
#define CY           (SCR_H / 2.0f)

/* ---- feel ----
 * Tuned so the ball carries real momentum: it overshoots junctions and you
 * can bury it in a dead end. Slow enough to be safe is not a game. */
#define BALL_R       6.0f
#define TICK_MS      16                /* ~60 Hz                        */
#define ACCEL_GAIN   180.0f            /* tilt -> px/s^2                */
#define DAMPING      0.9955f           /* momentum survives corners     */
#define RESTITUTION  0.42f
#define MAX_SPEED    1500.0f
#define GOAL_R       20.0f
#define BTN_GPIO     9                 /* BOOT, idles HIGH              */
#define HOLD_MS      400               /* short press = page, hold = talk */

/* ---- screen sleep ----
 * On AMOLED, brightness 0 actually darkens the pixels, so this is real power
 * saving and not a black rectangle. The framebuffer survives, so waking is
 * instant with nothing to redraw.
 *
 * Wake sources are the button and the IMU. Touch is deliberately not relied
 * on: it answers on roughly one boot in five on this unit, and a device you
 * cannot wake is worse than one that never slept. */
#define DIM_AFTER_MS   25000
#define OFF_AFTER_MS   75000
#define DIM_LEVEL      10
#define FULL_LEVEL     100
#define MOTION_THRESH  0.9f            /* m/s^2 of change counts as "picked up" */

/* Board axes -> screen axes. Horizontal reads inverted on this board. */
#define AXIS_X_SRC(gx, gy)   (-(gy))
#define AXIS_Y_SRC(gx, gy)   ( (gx))

/* IMU die sits above room temperature; trim toward ambient. */
#define TEMP_OFFSET_C  (-3.0f)

/* ---- palettes ----
 *
 * Two sets, swapped at runtime. Dark uses TRUE BLACK for the ground: on AMOLED
 * a black pixel is an unlit pixel, so this is the setting that actually costs
 * less power rather than merely looking darker. */
typedef enum {
    ROLE_BG, ROLE_CARD, ROLE_INK, ROLE_TEXT,
    ROLE_BALL, ROLE_GOAL, ROLE_GOALBG, ROLE_HALO, ROLE_ARENA, ROLE_RING, ROLE_ORB, ROLE_ORBG,
    ROLE_FACE, ROLE_GLINT, ROLE_BLUSH,
} role_t;

typedef struct {
    uint32_t bg, card, ink, text, ball, goal, goalbg, halo, arena, ring, orb, orbg,
             face, glint, blush;
} palette_t;

static const palette_t PAL_LIGHT = {
    .bg = 0xF3F5FA, .card = 0xFFFFFF, .ink = 0x39456F, .text = 0x5A6488,
    .ball = 0xE2513A, .goal = 0x2FA36B, .goalbg = 0xCDEBDC, .halo = 0xDCE4FA,
    .arena = 0xC6D1E8, .ring = 0x39456F, .orb = 0x4C7DF0, .orbg = 0x9B6BFF,
    .face = 0x9A5A12, .glint = 0xFFFFFF, .blush = 0xE8746B,
};
static const palette_t PAL_DARK = {
    .bg = 0x000000, .card = 0x121826, .ink = 0xC9D4F0, .text = 0x8E9AC2,
    .ball = 0xFF7A5C, .goal = 0x4FD69B, .goalbg = 0x123024, .halo = 0x161E33,
    .arena = 0x223357, .ring = 0x5B9CFF, .orb = 0x6E9BFF, .orbg = 0xA97BFF,
    .face = 0xFFC46B, .glint = 0xFFF3DE, .blush = 0xFF7E6B,
};
static palette_t P;

/* Objects register the role they play, so a theme change restyles them
   without a reboot and without threading colours through every builder. */
#define MAX_THEMED 64
static struct { lv_obj_t *o; role_t r; bool text; } s_themed[MAX_THEMED];
static int s_themed_n;

/* The settings rows are painted per-state rather than through the themed()
   registry, so a theme swap has to repaint them by hand. */
static lv_obj_t  *set_layer, *mode_row[3], *mode_lbl[3], *set_foot;
static void mode_paint(void);
static void paint_status(void);
static void touch_probe(void);
static void health_log(void);

static void theme_apply_parts(void);

static uint32_t role_colour(role_t r)
{
    switch (r) {
    case ROLE_BG:     return P.bg;
    case ROLE_CARD:   return P.card;
    case ROLE_INK:    return P.ink;
    case ROLE_TEXT:   return P.text;
    case ROLE_BALL:   return P.ball;
    case ROLE_GOAL:   return P.goal;
    case ROLE_GOALBG: return P.goalbg;
    case ROLE_HALO:   return P.halo;
    case ROLE_ARENA:  return P.arena;
    case ROLE_RING:   return P.ring;
    case ROLE_FACE:   return P.face;
    case ROLE_GLINT:  return P.glint;
    case ROLE_BLUSH:  return P.blush;
    case ROLE_ORB:    return P.orb;
    default:          return P.orbg;
    }
}

static lv_obj_t *themed(lv_obj_t *o, role_t r, bool is_text)
{
    if (o == NULL) return o;
    if (s_themed_n < MAX_THEMED) {
        s_themed[s_themed_n].o = o;
        s_themed[s_themed_n].r = r;
        s_themed[s_themed_n].text = is_text;
        s_themed_n++;
    }
    const lv_color_t c = lv_color_hex(role_colour(r));
    if (is_text) lv_obj_set_style_text_color(o, c, 0);
    else         lv_obj_set_style_bg_color(o, c, 0);
    return o;
}

static void theme_apply(bool dark)
{
    P = dark ? PAL_DARK : PAL_LIGHT;
    for (int i = 0; i < s_themed_n; i++) {
        const lv_color_t c = lv_color_hex(role_colour(s_themed[i].r));
        if (s_themed[i].text) lv_obj_set_style_text_color(s_themed[i].o, c, 0);
        else                  lv_obj_set_style_bg_color(s_themed[i].o, c, 0);
    }
    lv_obj_set_style_bg_color(lv_screen_active(), lv_color_hex(P.bg), 0);

    /* Arcs and borders live on parts the registry does not cover, and their
       objects are declared further down. */
    theme_apply_parts();
    if (mode_row[0]) mode_paint();
}

typedef enum { UI_IDLE, UI_LISTENING, UI_THINKING, UI_SPEAKING } ui_state_t;

/* Idle has two peer pages. A short press cycles them; a long hold talks. */
typedef enum { PAGE_GAME, PAGE_FACE, PAGE_FEED, PAGE_SETTINGS, PAGE_COUNT } page_t;

static ui_state_t ui_state = UI_IDLE;
/* Hands free listens only after someone asks it to, and only until they ask it
   to stop. Not persisted: a mic that reopens itself across a power cycle is
   not something anyone consented to. */
static bool hf_on;
static lv_obj_t  *game_layer, *face_layer, *feed_layer, *voice_layer, *voice_lbl, *voice_core;
static lv_obj_t  *eye_l, *eye_r, *mouth, *face_text, *face_status;
static lv_obj_t  *answer_layer, *answer_text;
static page_t     page = PAGE_GAME;
static lv_obj_t  *ball_obj, *goal_obj, *temp_lbl, *hint_lbl, *net_dot, *s_arena, *s_card;
static lv_obj_t  *mic_dot;
static lv_obj_t  *feed_msg, *feed_detail;

typedef struct {
    float     radius;
    float     gap_deg;      /* centre angle of the opening */
    float     half_deg;     /* half-width of the opening    */
    lv_obj_t *arc;
} ring_t;

static ring_t rings[RINGS];

static float bx, by, vx, vy;
static bool  solved;
static uint32_t solved_at;

static qmi8658_dev_t imu;
static bool          imu_ok;

/* Whatever orientation the board rests in IS level.
 *
 * Measured on the bench: lying flat on a desk the QMI8658 reports gravity on
 * Z (-9.19) with a standing in-plane residual of about (1.3, 1.9) m/s2 -- some
 * of it the desk, some of it sensor offset. Untreated that is a permanent
 * downhill that pins the ball against the outer wall and never lets go, which
 * reads exactly like magnetism. Subtracting the resting vector means only a
 * DELIBERATE tilt moves the ball. */
static float bias_x, bias_y;

static uint32_t last_activity;
static int      screen_level = FULL_LEVEL;
static float    last_temp_c;
static float    prev_gx, prev_gy, prev_gz;
static bool     have_prev;

/* Boot levelling goes stale the moment the board is picked up, so switching
   TO the game page re-levels against however it is being held now. Done as a
   countdown inside the physics tick rather than a blocking sample loop, which
   would stall LVGL. */
static int   relevel = 0;
static float rl_sx, rl_sy;
#define RELEVEL_SAMPLES 30
/* Levelling captured in one orientation is meaningless in another: levelled
   on its edge and then laid flat, the reference is a whole g out and the ball
   is shoved to one side permanently. If the board reads a large sustained
   tilt while being held still, the reference is stale rather than the user
   holding it at 40 degrees without moving. */
#define STALE_TILT   6.0f     /* m/s^2 in-plane, roughly 37 degrees */
#define STALE_TICKS  90       /* ~1.5s at 60Hz                      */

static inline float clampf(float v, float lo, float hi)
{ return v < lo ? lo : (v > hi ? hi : v); }

static void set_screen(int level)
{
    if (level == screen_level) return;
    ESP_LOGI(TAG, "screen %d%% -> %d%%", screen_level, level);
    screen_level = level;
    bsp_display_brightness_set(level);
}

static void note_activity(void)
{
    last_activity = lv_tick_get();
    if (screen_level != FULL_LEVEL) set_screen(FULL_LEVEL);
}

static void anim_size(void *obj, int32_t v)
{ lv_obj_set_size((lv_obj_t *)obj, v, v); lv_obj_center((lv_obj_t *)obj); }

/* ------------------------------------------------------------------ maze */

/* The ring arcs and the arena outline, restyled on a theme swap. */
static void theme_apply_parts(void)
{
    for (int i = 0; i < RINGS; i++)
        if (rings[i].arc)
            lv_obj_set_style_arc_color(rings[i].arc, lv_color_hex(P.ring),
                                       LV_PART_INDICATOR);
    if (s_arena) lv_obj_set_style_border_color(s_arena, lv_color_hex(P.arena), 0);
    if (s_card)  lv_obj_set_style_border_color(s_card,  lv_color_hex(P.arena), 0);
}

static float norm360(float a)
{
    while (a < 0.0f)    a += 360.0f;
    while (a >= 360.0f) a -= 360.0f;
    return a;
}

/* Is `ang` inside the ring's opening? */
static bool in_gap(const ring_t *r, float ang)
{
    float d = fabsf(norm360(ang - r->gap_deg));
    if (d > 180.0f) d = 360.0f - d;
    return d < r->half_deg;
}

static void rings_generate(void)
{
    /* Outermost leaves a starting corridor; innermost encloses the goal. */
    const float outer = 145.0f, inner = 40.0f;
    const float step  = (outer - inner) / (RINGS - 1);

    for (int i = 0; i < RINGS; i++) {
        ring_t *r = &rings[i];
        r->radius   = outer - step * i;
        /* Constant pixel opening -> equal difficulty at every radius. */
        r->half_deg = (GAP_ARC_PX / 2.0f) / (r->radius * (float)M_PI / 180.0f);
        r->gap_deg  = (float)(esp_random() % 360u);
    }
}

static void rings_draw(void)
{
    for (int i = 0; i < RINGS; i++) {
        ring_t *r = &rings[i];
        if (r->arc == NULL) {
            r->arc = lv_arc_create(game_layer);
            /* Strip ALL default-theme styling, not just the knob.
             *
             * Removing only LV_PART_KNOB left the MAIN part carrying LVGL's
             * default theme, which paints a pale background track -- the
             * "circles are still white" on a black ground. Nothing here should
             * inherit a palette this project does not control. */
            lv_obj_remove_style_all(r->arc);
            lv_obj_set_style_arc_opa(r->arc, LV_OPA_TRANSP, LV_PART_MAIN);
            lv_obj_set_style_bg_opa(r->arc, LV_OPA_TRANSP, LV_PART_MAIN);
            lv_obj_set_style_arc_width(r->arc, RING_T, LV_PART_INDICATOR);
            lv_obj_set_style_arc_color(r->arc, lv_color_hex(P.ring), LV_PART_INDICATOR);
            lv_obj_set_style_arc_rounded(r->arc, true, LV_PART_INDICATOR);
            lv_obj_remove_flag(r->arc, LV_OBJ_FLAG_CLICKABLE);
        }
        const int d = (int)(r->radius * 2.0f) + RING_T;
        lv_obj_set_size(r->arc, d, d);
        lv_obj_center(r->arc);
        lv_arc_set_bg_angles(r->arc, 0, 360);
        /* Draw everything except the opening. */
        lv_arc_set_angles(r->arc,
                          (int)norm360(r->gap_deg + r->half_deg),
                          (int)(r->gap_deg - r->half_deg + 360.0f));
    }
}

static float goal_cx(void) { return CX; }
static float goal_cy(void) { return CY; }

/* --------------------------------------------------------------- physics */

static void reset_ball(void)
{
    bx = CX;                    /* outer corridor, outside the outermost ring */
    by = CY - 160.0f;   /* outer corridor, inside the arena */
    vx = vy = 0.0f;
    solved = false;
}

/* Push the ball out of one gap end, treated as a round post. Without this
   the ball clips the edge of an opening instead of catching on it, which
   reads as the wall being a suggestion. */
static void collide_post(float px, float py)
{
    const float dx = bx - px, dy = by - py;
    const float rr = BALL_R + RING_T / 2.0f;
    const float d2 = dx * dx + dy * dy;
    if (d2 >= rr * rr || d2 < 0.0001f) return;

    const float d = sqrtf(d2), nx = dx / d, ny = dy / d;
    bx += nx * (rr - d);
    by += ny * (rr - d);
    const float vn = vx * nx + vy * ny;
    if (vn < 0.0f) {
        vx -= (1.0f + RESTITUTION) * vn * nx;
        vy -= (1.0f + RESTITUTION) * vn * ny;
    }
}

static void collide(void)
{
    for (int i = 0; i < RINGS; i++) {
        const ring_t *r = &rings[i];
        const float dx = bx - CX, dy = by - CY;
        const float d = sqrtf(dx * dx + dy * dy);
        if (d < 0.001f) continue;

        const float reach = BALL_R + RING_T / 2.0f;
        const float ang = norm360(atan2f(dy, dx) * 180.0f / (float)M_PI);

        if (fabsf(d - r->radius) < reach && !in_gap(r, ang)) {
            const float nx = dx / d, ny = dy / d;
            const float sign = (d > r->radius) ? 1.0f : -1.0f;   /* eject the way we came */
            const float target = r->radius + sign * reach;
            bx = CX + nx * target;
            by = CY + ny * target;
            const float vn = vx * nx + vy * ny;
            if (vn * sign < 0.0f) {
                vx -= (1.0f + RESTITUTION) * vn * nx;
                vy -= (1.0f + RESTITUTION) * vn * ny;
            }
        }

        /* The two ends of the opening. */
        for (int e = 0; e < 2; e++) {
            const float a = (r->gap_deg + (e ? r->half_deg : -r->half_deg))
                          * (float)M_PI / 180.0f;
            collide_post(CX + cosf(a) * r->radius, CY + sinf(a) * r->radius);
        }
    }
}

static bool answer_is_open(void);
static void answer_close(void);

static void show_layer(lv_obj_t *o, bool on)
{
    if (o == NULL) return;
    if (on) lv_obj_remove_flag(o, LV_OBJ_FLAG_HIDDEN);
    else    lv_obj_add_flag(o, LV_OBJ_FLAG_HIDDEN);
}

static void apply_layers(void)
{
    const bool idle = (ui_state == UI_IDLE);
    /* The face IS the voice interface, so a turn does not replace it -- it
       animates it. Only the game and feed pages hand over to the orb. */
    const bool face_on = (page == PAGE_FACE);
    show_layer(game_layer,  idle && page == PAGE_GAME);
    show_layer(face_layer,  face_on);
    show_layer(feed_layer,  idle && page == PAGE_FEED);
    show_layer(set_layer,   idle && page == PAGE_SETTINGS);
    show_layer(voice_layer, !idle && !face_on);
    if (!idle || page != PAGE_FACE) answer_close();
}

static void set_state(ui_state_t st)
{
    if (st == ui_state) return;
    const ui_state_t prev = ui_state;
    ui_state = st;
    apply_layers();
    if (st == UI_LISTENING) {
        /* Hands-free may already own the microphone. Opening a second capture
           would fail silently; forcing its gate is what the press means. */
        if (audio_capturing()) audio_open_gate();
        else                   audio_start();
        voice_note_event("media.voice_note.recording_started", "esp_terminal");
    } else if (prev == UI_LISTENING) {
        audio_stop();
        voice_submit();
    }

    switch (st) {
    case UI_LISTENING: lv_label_set_text(voice_lbl, "opening mic"); break;
    case UI_THINKING:  lv_label_set_text(voice_lbl, "thinking");  break;
    case UI_SPEAKING:  lv_label_set_text(voice_lbl, "speaking");  break;
    case UI_IDLE: break;
    }
}

static void cycle_page(void)
{
    page = (page_t)((page + 1) % PAGE_COUNT);
    if (page == PAGE_GAME) {          /* re-level on entry */
        relevel = RELEVEL_SAMPLES;
        rl_sx = rl_sy = 0.0f;
        vx = vy = 0.0f;
    }
    if (page != PAGE_FACE) {
        /* Mid-utterance, leaving submits what was already said rather than
           discarding it -- the words were spoken, they should count. */
        if (ui_state == UI_LISTENING) set_state(UI_IDLE);
        else                          audio_disarm();
        realtime_stop();          /* a call does not outlive its own screen */
    }
    apply_layers();
    ESP_LOGI(TAG, "page -> %s", page == PAGE_GAME ? "game" : page == PAGE_FACE ? "face"
                              : page == PAGE_FEED ? "feed" : "settings");
}

/* NOTE for the audio work: this drives the screen from the BUTTON, which is
   only honest while there is no microphone. Once capture exists, LISTENING
   must be entered on the first I2S buffer instead -- the screen has to report
   that the mic is live, not that a finger moved. */
static void poll_button(void)
{
    static int stable = 1, pending = 1, agree = 0;
    static uint32_t press_at = 0;
    static bool talked = false;

    const int raw = gpio_get_level(BTN_GPIO);
    if (raw == pending) { if (agree < 2) agree++; }
    else                { pending = raw; agree = 0; }

    static bool woke_only = false;

    if (agree >= 2 && stable != pending) {
        stable = pending;
        if (stable == 0) {                 /* pressed  (active LOW) */
            /* Consume the press that wakes a dark screen. Acting on it would
               switch pages or open a mic the user never asked for. Dimmed is
               still readable, so only a fully dark screen consumes. */
            woke_only = (screen_level == 0);
            note_activity();
            press_at = lv_tick_get();
            talked = false;
        } else {                           /* released */
            note_activity();
            if (woke_only)          woke_only = false;
            else if (talked)        set_state(UI_IDLE);
            else if (answer_is_open()) answer_close();  /* press backs out */
            else                    cycle_page();
        }
    }

    /* Holding elsewhere does nothing and the release cycles the page, which is
       the same rule as hands free: voice lives on the face and nowhere else. */
    if (stable == 0 && !woke_only && !talked && page == PAGE_FACE &&
        lv_tick_elaps(press_at) >= HOLD_MS) {
        talked = true;
        /* Realtime already owns the microphone and its control WebSocket. A
           second push-to-talk capture races that owner, fails its request, and
           paints a red connection error even though the live call is healthy.
           Consume the hold; a short press still changes page and ends the call
           through cycle_page(). */
        if (net_voice_mode() == MODE_REALTIME)
            ESP_LOGI(TAG, "button: hold ignored while realtime owns the mic");
        else
            set_state(UI_LISTENING);
    }
    if (stable == 0) note_activity();      /* held down is still activity */
}

/* Hands-free: the device holds the button.
 *
 * The mic is armed only while nothing else owns it and nothing is coming out
 * of the speaker -- a terminal that listens through its own reply answers
 * itself. Everything downstream of the endpoint is the push-to-talk path
 * unchanged, so there is one upload, one transport and one bug surface. */
static void handsfree_tick(void)
{
    /* Checked first and unconditionally: this also rescues a push-to-talk
       recording that ran into the length cap while the button was still
       down, which used to leave the screen listening to a dead capture. */
    if (ui_state == UI_LISTENING) {
        if (audio_utterance_done()) set_state(UI_IDLE);   /* stops, submits */
        return;
    }
    if (ui_state != UI_IDLE) return;

    const turn_state_t ts = voice_state();
    const bool busy = (ts == TURN_SENDING || ts == TURN_THINKING ||
                       ts == TURN_SPEAKING || audio_speaker_busy());
    /* The face is the voice interface. Everywhere else the microphone is shut
       and the audio hardware is released -- a mic that stays open behind a
       maze or a settings list is one nobody is thinking about. */
    if (page != PAGE_FACE || net_voice_mode() != MODE_HANDSFREE || !hf_on || busy ||
        realtime_state() != RT_OFF) {
        audio_disarm();                       /* no-op unless armed and mute */
        return;
    }

    if (!audio_capturing())    audio_arm();
    else if (audio_speech_started()) set_state(UI_LISTENING);
}

static void tick(lv_timer_t *t)
{
    LV_UNUSED(t);
    poll_button();
    touch_probe();
    handsfree_tick();
    audio_idle_tick();

    {   /* every 10 s, unconditionally -- heat needs a series, not a snapshot */
        static uint32_t last_health;
        if (lv_tick_elaps(last_health) > 10000) { last_health = lv_tick_get(); health_log(); }
    }

    /* A dark screen has no audience. The IMU still has to be read, because it
       is the wake source, but a quarter as often is plenty for "was this
       picked up" -- and the physics, the face and the redraw behind them are
       work nobody can see. This part runs at 60 Hz for hours at a stretch. */
    if (screen_level == 0) {
        static int slow;
        if (++slow & 3) return;
    }

    /* Read the IMU regardless of page: it is both the tilt input AND the
       wake source, and a device that only notices motion on one screen is
       not much of a wake source. */
    float gx = 0, gy = 0, gz = 0;
    const bool have_imu = imu_ok && qmi8658_read_accel(&imu, &gx, &gy, &gz) == ESP_OK;
    if (have_imu) {
        if (have_prev) {
            const float dm = fabsf(gx - prev_gx) + fabsf(gy - prev_gy) + fabsf(gz - prev_gz);
            if (dm > MOTION_THRESH) note_activity();
        }
        prev_gx = gx; prev_gy = gy; prev_gz = gz;
        have_prev = true;
    }
    if (ui_state == UI_LISTENING) {
        /* Promote the label only when the microphone is genuinely delivering
           audio -- §8's rule, finally against a real codec. */
        lv_label_set_text(voice_lbl,
                          audio_live() ? "listening" :
                          audio_ready() ? "opening mic" : "no microphone");
    }

    /* A finished turn pulls the feed page forward once, on the transition
       only. Doing it every render would yank the game away under the user's
       hands each time the physics ticked. */
    {
        static turn_state_t seen = TURN_IDLE;
        const turn_state_t ts = voice_state();
        if (ts != seen) {
            if (seen == TURN_IDLE && ts != TURN_IDLE && page != PAGE_FACE) {
                page = PAGE_FACE;          /* answers belong on the face */
                apply_layers();
            }
            seen = ts;
            note_activity();
        }
    }

    /* Touch, when the panel happens to be answering, counts too. */
    if (lv_display_get_inactive_time(NULL) < 200) note_activity();

    /* Never sleep mid-turn: a screen that goes dark while the mic is open is
       precisely the indicator lie this project keeps guarding against.
       `ui_state` alone is not the test. In hands free nobody is holding a
       button, so ui_state is IDLE for the whole of sending, thinking and
       speaking -- and the screen went dark over the answer while the device
       read it aloud. The turn, not the finger, decides. */
    {
        const turn_state_t ts_live = voice_state();
        /* Armed-and-waiting is NOT a turn. Keying this off audio_capturing()
           would hold the panel at full brightness for as long as hands free is
           on, which is the opposite of what an always-listening device should
           do to a battery. */
        const bool mid_turn = ui_state != UI_IDLE || audio_speech_started() ||
                              ts_live == TURN_SENDING || ts_live == TURN_THINKING ||
                              ts_live == TURN_SPEAKING;
        if (mid_turn) note_activity();
    }
    if (ui_state == UI_IDLE) {
        const uint32_t idle_ms = lv_tick_elaps(last_activity);
        if      (idle_ms > OFF_AFTER_MS) set_screen(0);
        else if (idle_ms > DIM_AFTER_MS) set_screen(DIM_LEVEL);
    } else {
        note_activity();
    }

    if (ui_state != UI_IDLE || page != PAGE_GAME) return;

    const float dt = TICK_MS / 1000.0f;
    if (!solved) {
        float ax = 0, ay = 0;
        if (have_imu) {
            {
                if (relevel > 0) {
                    rl_sx += gx;
                    rl_sy += gy;
                    if (--relevel == 0) {
                        bias_x = rl_sx / RELEVEL_SAMPLES;
                        bias_y = rl_sy / RELEVEL_SAMPLES;
                        ESP_LOGI(TAG, "re-levelled: bias (%.2f, %.2f)", bias_x, bias_y);
                    }
                    vx = vy = 0.0f;      /* hold still while sampling */
                    return;
                }
                const float tx = gx - bias_x, ty = gy - bias_y;

                /* Still, yet reading a steep slope -> the reference is wrong. */
                static int stale = 0;
                const float mag = sqrtf(tx * tx + ty * ty);
                const float moved = fabsf(gx - prev_gx) + fabsf(gy - prev_gy)
                                  + fabsf(gz - prev_gz);
                if (mag > STALE_TILT && moved < 0.35f) {
                    if (++stale > STALE_TICKS) {
                        ESP_LOGI(TAG, "stale level (%.1f m/s2 while still) -- re-levelling", mag);
                        relevel = RELEVEL_SAMPLES;
                        rl_sx = rl_sy = 0.0f;
                        stale = 0;
                    }
                } else {
                    stale = 0;
                }
                ax = AXIS_X_SRC(tx, ty) * ACCEL_GAIN;
                ay = AXIS_Y_SRC(tx, ty) * ACCEL_GAIN;

                /* Instrumentation: latch the extremes so the board can be
                   tilted at any time and the range read back afterwards.
                   Guessing at feel twice was enough. */
                static float lox = 9e9f, hix = -9e9f, loy = 9e9f, hiy = -9e9f;
                static float lod = 9e9f, hid = -9e9f, hisp = 0;
                static int   n = 0;
                if (gx < lox) lox = gx;
                if (gx > hix) hix = gx;
                if (gy < loy) loy = gy;
                if (gy > hiy) hiy = gy;
                const float dd = sqrtf((bx-CX)*(bx-CX) + (by-CY)*(by-CY));
                if (dd < lod) lod = dd;
                if (dd > hid) hid = dd;
                const float spd = sqrtf(vx*vx + vy*vy);
                if (spd > hisp) hisp = spd;
                if (++n % 60 == 0)
                    ESP_LOGI(TAG,
                        "accel x[%.2f..%.2f] y[%.2f..%.2f] now(%.2f,%.2f,%.2f) | "
                        "ball d[%.0f..%.0f] now %.0f  vmax %.0f",
                        lox, hix, loy, hiy, gx, gy, gz, lod, hid, dd, hisp);
            }
        }
        vx = (vx + ax * dt) * DAMPING;
        vy = (vy + ay * dt) * DAMPING;
        const float sp = sqrtf(vx * vx + vy * vy);
        if (sp > MAX_SPEED) { vx = vx / sp * MAX_SPEED; vy = vy / sp * MAX_SPEED; }

        bx += vx * dt; by += vy * dt;

        /* Arena wall. Circular, so no corner exists to lose the ball in. */
        {
            const float dx = bx - CX, dy = by - CY;
            const float d = sqrtf(dx * dx + dy * dy);
            const float lim = ARENA_R - BALL_R;
            if (d > lim && d > 0.001f) {
                const float nx = dx / d, ny = dy / d;
                bx = CX + nx * lim;
                by = CY + ny * lim;
                const float vn = vx * nx + vy * ny;
                if (vn > 0.0f) {
                    vx -= (1.0f + RESTITUTION) * vn * nx;
                    vy -= (1.0f + RESTITUTION) * vn * ny;
                }
            }
        }

        /* Two passes: at this speed a single pass can leave the ball resting
           inside a corner it was pushed into by the other wall. */
        collide();
        collide();

        const float dx = bx - goal_cx(), dy = by - goal_cy();
        if (sqrtf(dx * dx + dy * dy) < GOAL_R) {
            solved = true;
            solved_at = lv_tick_get();
            lv_label_set_text(hint_lbl, "solved");
        }
    } else if (lv_tick_elaps(solved_at) > 1600) {
        /* Fresh gap angles every win, so it never becomes muscle memory. */
        rings_generate();
        rings_draw();
        lv_label_set_text(hint_lbl, "tilt to the core  .  press to switch");
        reset_ball();
        relevel = RELEVEL_SAMPLES;     /* the board has usually moved by now */
        rl_sx = rl_sy = 0.0f;
    }
    lv_obj_set_pos(ball_obj, (int)(bx - BALL_R), (int)(by - BALL_R));
}

/* The feed page has exactly one writer.
 *
 * It previously had two -- the network poller and the turn handler -- both
 * setting the same two labels on their own timers. They overwrote each other
 * mid-turn, which is what made the screen flicker between states. Rendering
 * from a single priority removes the race rather than staggering it. */
static void set_feed(const char *head, const char *detail)
{
    if (feed_msg == NULL) return;
    /* Only touch LVGL when the text actually changed; repainting identical
       strings several times a second is its own source of flicker. */
    if (strcmp(lv_label_get_text(feed_msg), head) != 0) {
        lv_label_set_text(feed_msg, head);
        lv_obj_align(feed_msg, LV_ALIGN_CENTER, 0, -60);
    }
    if (strcmp(lv_label_get_text(feed_detail), detail) != 0) {
        lv_label_set_text(feed_detail, detail);
        lv_obj_align(feed_detail, LV_ALIGN_CENTER, 0, 40);
    }
}

static void feed_tick(lv_timer_t *t)
{
    LV_UNUSED(t);

    if (mic_dot != NULL) {
        const bool open = audio_capturing();
        if (open) lv_obj_remove_flag(mic_dot, LV_OBJ_FLAG_HIDDEN);
        else      lv_obj_add_flag(mic_dot, LV_OBJ_FLAG_HIDDEN);
        /* Hollow while it is merely open, solid once it is keeping what it
           hears. Two different things, so they do not look alike. */
        lv_obj_set_style_bg_color(mic_dot,
            lv_color_hex(audio_speech_started() ? 0xE0565C : 0x8A6A2F), 0);
    }
    if (net_dot != NULL) {
        uint32_t c;
        switch (net_state()) {
        case NET_ONLINE:                        c = 0x2FA36B; break;  /* green  */
        case NET_CONNECTING: case NET_BOOTING:  c = 0xE0902F; break;  /* orange */
        case NET_PROVISIONING:                  c = 0xE0902F; break;
        default:                                c = 0xD4442C; break;  /* red    */
        }
        lv_obj_set_style_bg_color(net_dot, lv_color_hex(c), 0);
    }

    if (feed_msg == NULL) return;

    /* This page is For You and nothing else.
     *
     * It used to double as the turn readout, which meant the answer to a
     * question you just asked appeared under a heading about your feed. The
     * face owns the conversation now; this shows the latest item, or says
     * plainly that there is none. */
    if (net_state() == NET_ONLINE) {
        /* No feed client yet: the endpoints exist server-side but this device
           does not read them, and inventing an item would be worse than an
           empty shelf. */
        set_feed("nothing new", "for you items appear here");
        return;
    }

    const char *head;
    switch (net_state()) {
    case NET_PROVISIONING: head = "set up wifi";          break;
    case NET_CONNECTING:   head = "connecting";           break;
    case NET_NO_WIFI:      head = "wifi failed";          break;
    case NET_NO_BACKEND:   head = "magician unreachable"; break;
    default:               head = "starting";             break;
    }
    /* Only while something is wrong is the connection detail worth the space. */
    char line[160];
    const char *ip = net_ip();
    snprintf(line, sizeof line, "%s\n\nhelp: http://%s/help",
             net_detail(), ip[0] ? ip : "192.168.4.1");
    set_feed(head, line);
}

static void temp_tick(lv_timer_t *t)
{
    LV_UNUSED(t);
    float c;
    if (imu_ok && qmi8658_read_temp(&imu, &c) == ESP_OK) {
        /* LVGL's own printf does not implement %f unless LV_SPRINTF_USE_FLOAT
           is enabled, which is why this rendered as "fC". Integer tenths need
           no float formatting at all. */
        const int t10 = (int)lroundf((c + TEMP_OFFSET_C) * 10.0f);
        last_temp_c = c + TEMP_OFFSET_C;
        char buf[16];
        snprintf(buf, sizeof buf, "%d.%d C", t10 / 10, abs(t10 % 10));
        lv_label_set_text(temp_lbl, buf);
    } else {
        lv_label_set_text(temp_lbl, "-- C");
    }
}

/* -------------------------------------------------------------------- ui */

static lv_obj_t *full_layer(lv_obj_t *parent)
{
    lv_obj_t *o = lv_obj_create(parent);
    lv_obj_remove_style_all(o);
    lv_obj_set_size(o, SCR_W, SCR_H);
    lv_obj_set_pos(o, 0, 0);
    return o;
}

/* The For You page.
 *
 * There is no network client yet, so this states plainly that it is not
 * connected rather than rendering placeholder items. A feed that invents
 * content is the same defect as an indicator that invents a microphone.
 * Dismiss / helpful gestures land here once real items do -- touch swipe
 * when the panel answers at boot, tilt as the fallback, since touch is
 * intermittent on this unit and the IMU is not. */
static void build_feed_layer(lv_obj_t *parent)
{
    feed_layer = full_layer(parent);
    lv_obj_add_flag(feed_layer, LV_OBJ_FLAG_HIDDEN);
    themed(feed_layer, ROLE_BG, false);
    lv_obj_set_style_bg_opa(feed_layer, LV_OPA_COVER, 0);

    lv_obj_t *title = lv_label_create(feed_layer);
    themed(title, ROLE_INK, true);
    lv_obj_set_style_text_font(title, &lv_font_montserrat_32, 0);
    lv_label_set_text(title, "For you");
    lv_obj_align(title, LV_ALIGN_TOP_MID, 0, 52);

    lv_obj_t *card = lv_obj_create(feed_layer);
    lv_obj_remove_style_all(card);
    lv_obj_set_size(card, SCR_W - 36, 250);
    lv_obj_center(card);
    lv_obj_set_style_radius(card, 16, 0);
    themed(card, ROLE_CARD, false);
    lv_obj_set_style_bg_opa(card, LV_OPA_COVER, 0);
    lv_obj_set_style_border_width(card, 1, 0);
    /* Was a literal, so it stayed light on the dark card. */
    lv_obj_set_style_border_color(card, lv_color_hex(P.arena), 0);
    s_card = card;

    feed_msg = lv_label_create(card);
    lv_label_set_long_mode(feed_msg, LV_LABEL_LONG_WRAP);
    lv_obj_set_width(feed_msg, SCR_W - 72);
    lv_obj_set_style_text_align(feed_msg, LV_TEXT_ALIGN_CENTER, 0);
    themed(feed_msg, ROLE_INK, true);
    lv_obj_set_style_text_font(feed_msg, &lv_font_montserrat_32, 0);
    lv_label_set_text(feed_msg, "starting");
    lv_obj_align(feed_msg, LV_ALIGN_CENTER, 0, -60);

    /* Names the AP to join, or the URL that did not answer -- an error the
       user cannot act on is not worth showing. */
    feed_detail = lv_label_create(card);
    themed(feed_detail, ROLE_TEXT, true);
    lv_obj_set_style_text_font(feed_detail, &lv_font_montserrat_20, 0);
    lv_label_set_long_mode(feed_detail, LV_LABEL_LONG_WRAP);
    lv_obj_set_width(feed_detail, SCR_W - 72);
    lv_obj_set_style_text_align(feed_detail, LV_TEXT_ALIGN_CENTER, 0);
    lv_label_set_text(feed_detail, "");
    lv_obj_align(feed_detail, LV_ALIGN_CENTER, 0, 40);

    lv_obj_t *foot = lv_label_create(feed_layer);
    themed(foot, ROLE_TEXT, true);
    lv_obj_set_style_text_font(foot, &lv_font_montserrat_20, 0);
    lv_label_set_text(foot, "press to switch");
    lv_obj_align(foot, LV_ALIGN_BOTTOM_MID, 0, -14);
}

/* Retro pixel face.
 *
 * Square corners and chunky blocks throughout: the whole read is "made of
 * pixels", and a rounded eye would break it instantly. Everything is plain
 * rectangles, so this costs a handful of LVGL objects and no extra draw
 * buffer -- that buffer is scratch space sized by the panel, not by what is
 * drawn into it. */
/* Closer in: bigger features, less empty board. */
#define EYE_W       72
#define EYE_OPEN    52          /* wider than tall: a square stare reads cold */
#define EYE_SHUT    10
#define EYE_Y       170   /* low on the face: the big forehead is the cue */
#define EYE_DX      100  /* wide-set reads younger */            /* centre offset of each eye */
#define BROW_W      74
#define BROW_H      12
#define MOUTH_W     108
#define MOUTH_MIN   18
#define MOUTH_MAX   88
#define MOUTH_Y     252
/* The mouth is a centre bar plus two raised corner blocks. Flat reads grumpy;
   lifting the ends by one chunky pixel is the whole difference, and keeping
   them as separate blocks preserves the pixel look a curve would lose. */
#define CORNER_W    26
#define CORNER_H    18
#define SMILE_LIFT  12
#define GLINT       18
#define BLUSH_W     34
#define BLUSH_H     16
#define BOB_PX      3        /* idle drift, small enough to feel rather than see */

static lv_obj_t *brow_l, *brow_r, *corner_l, *corner_r, *glint_l, *glint_r;
static lv_obj_t *blush_l, *blush_r;

static lv_obj_t *block(lv_obj_t *parent, int w, int h, role_t role)
{
    lv_obj_t *o = lv_obj_create(parent);
    lv_obj_remove_style_all(o);
    lv_obj_set_size(o, w, h);
    lv_obj_set_style_radius(o, 0, 0);          /* pixels have corners */
    lv_obj_set_style_bg_opa(o, LV_OPA_COVER, 0);
    /* Blocks are decoration. Left clickable, they swallow the tap meant for
       the page beneath and the answer never opens. */
    lv_obj_remove_flag(o, LV_OBJ_FLAG_CLICKABLE);
    return themed(o, role, false);
}

static void eye_height(lv_obj_t *e, int h)
{
    /* Grow and shrink about the centre so a blink closes like a lid rather
       than sliding the eye upward. */
    lv_obj_set_size(e, EYE_W, h);
    lv_obj_set_y(e, EYE_Y + (EYE_OPEN - h) / 2);
}

static void anim_blink(void *obj, int32_t v)
{
    lv_obj_t *e = (lv_obj_t *)obj;
    eye_height(e, v);
    lv_obj_t *g = (e == eye_l) ? glint_l : glint_r;
    if (g) {
        if (v < GLINT + 16) lv_obj_add_flag(g, LV_OBJ_FLAG_HIDDEN);
        else                lv_obj_remove_flag(g, LV_OBJ_FLAG_HIDDEN);
    }
}

static void blink_tick(lv_timer_t *t)
{
    LV_UNUSED(t);
    if (face_layer == NULL || lv_obj_has_flag(face_layer, LV_OBJ_FLAG_HIDDEN)) return;
    if (ui_state == UI_LISTENING) return;       /* wide-eyed while hearing you */
    if ((esp_random() % 100u) >= 6) return;

    /* Blink in pairs, sometimes.
     *
     * One blink reads mechanical; a quick second beat reads alive. Pure
     * timing -- no extra objects, and the difference is out of proportion to
     * the change. */
    const bool twice = (esp_random() % 100u) < 45;
    for (int i = 0; i < 2; i++) {
        lv_anim_t a;
        lv_anim_init(&a);
        lv_anim_set_var(&a, i ? eye_r : eye_l);
        lv_anim_set_exec_cb(&a, anim_blink);
        lv_anim_set_values(&a, EYE_OPEN, EYE_SHUT);
        lv_anim_set_duration(&a, 70);
        lv_anim_set_playback_duration(&a, 90);
        lv_anim_set_repeat_count(&a, twice ? 2 : 1);
        lv_anim_set_repeat_delay(&a, 110);
        lv_anim_start(&a);
    }
}

/* Everything the face does is driven by real device state.
 *
 * Brows raise when the microphone is genuinely live, furrow while waiting on
 * Magician, and the mouth moves only while audio is actually playing. A face
 * that emotes on a timer would be telling the same lie the indicators
 * elsewhere are built to avoid. */
static void face_tick(lv_timer_t *t)
{
    LV_UNUSED(t);
    if (face_layer == NULL || lv_obj_has_flag(face_layer, LV_OBJ_FLAG_HIDDEN)) return;

    static int brow_y = EYE_Y - 34, mouth_h = MOUTH_MIN, phase, drift;
    static int bob, eye_h = EYE_OPEN;
    int brow_target = EYE_Y - 34, mouth_target = MOUTH_MIN, eye_dx = 0;
    int bob_target = 0, eye_target = EYE_OPEN;

    const turn_state_t ts = voice_state();
    if (ui_state == UI_LISTENING) {
        brow_target  = EYE_Y - 52;                       /* raised, attentive */
        mouth_target = MOUTH_MIN + 8;
    } else if (ts == TURN_SENDING || ts == TURN_THINKING) {
        brow_target  = EYE_Y - 22;                       /* furrowed          */
        mouth_target = MOUTH_MIN;
        eye_dx = ((phase / 8) % 2) ? 7 : -7;             /* eyes cast about   */
    } else if (ts == TURN_THINKING) {
        /* Anticipation: eyes widen a beat BEFORE the reply lands, so the face
           looks like it saw it coming rather than reacting late. */
        eye_target = EYE_OPEN + 10;
    } else if (ts == TURN_SPEAKING) {
        static const uint8_t shape[] = {0, 3, 6, 4, 7, 2, 5, 1};
        mouth_target = MOUTH_MIN + (MOUTH_MAX - MOUTH_MIN) * shape[phase % 8] / 7;
        brow_target  = EYE_Y - 38;
    } else {
        /* Idle: a slow lazy drift so it reads alive without being busy. */
        drift++;
        eye_dx = (drift / 40) % 4 == 1 ? 5 : (drift / 40) % 4 == 3 ? -5 : 0;
        /* A slow bob. Too small to notice; its absence is what makes a face
           look like a frozen graphic. */
        bob_target = ((drift / 25) % 2) ? BOB_PX : -BOB_PX;
    }
    phase++;

    /* Ease toward the targets; stepping straight to them looks mechanical. */
    brow_y  += (brow_target  - brow_y)  / 3;
    mouth_h += (mouth_target - mouth_h) / 2;
    bob     += (bob_target   - bob)     / 12;    /* slower than everything else */
    eye_h   += (eye_target   - eye_h)   / 4;

    lv_obj_set_pos(brow_l, SCR_W / 2 - EYE_DX - BROW_W / 2 + eye_dx, brow_y + bob);
    lv_obj_set_pos(brow_r, SCR_W / 2 + EYE_DX - BROW_W / 2 + eye_dx, brow_y + bob);
    lv_obj_set_x(eye_l, SCR_W / 2 - EYE_DX - EYE_W / 2 + eye_dx);
    lv_obj_set_x(eye_r, SCR_W / 2 + EYE_DX - EYE_W / 2 + eye_dx);
    /* Only drive eye height here when no blink animation owns it. */
    if (!lv_anim_get(eye_l, anim_blink)) {
        lv_obj_set_height(eye_l, eye_h);
        lv_obj_set_height(eye_r, eye_h);
        lv_obj_set_y(eye_l, EYE_Y + (EYE_OPEN - eye_h) / 2 + bob);
        lv_obj_set_y(eye_r, EYE_Y + (EYE_OPEN - eye_h) / 2 + bob);
    }
    if (blush_l) {
        lv_obj_set_y(blush_l, EYE_Y + EYE_OPEN + 14 + bob);
        lv_obj_set_y(blush_r, EYE_Y + EYE_OPEN + 14 + bob);
    }

    if (lv_obj_get_height(mouth) != mouth_h) {
        const int top = MOUTH_Y + (MOUTH_MAX - mouth_h) / 2;
        lv_obj_set_size(mouth, MOUTH_W, mouth_h);
        lv_obj_set_y(mouth, top);
        /* Corners ride the bar's top edge, so the smile survives the mouth
           opening instead of detaching from it. */
        lv_obj_set_y(corner_l, top - SMILE_LIFT);
        lv_obj_set_y(corner_r, top - SMILE_LIFT);
    }

    /* Once there is an answer, show the ANSWER. The transcript is only what
       the device heard, and it was sitting there where the reply belonged. */
    const char *txt;
    if (net_voice_mode() == MODE_REALTIME) {
        switch (realtime_state()) {
        case RT_OPENING:  txt = "connecting";       break;
        case RT_LIVE:     txt = "just talk";        break;
        case RT_SPEAKING: txt = "replying";         break;
        case RT_FAILED:   txt = realtime_error();   break;
        default:          txt = "tap face to call"; break;
        }
    }
    else if (ts == TURN_ERROR)               txt = voice_error();
    /* Only claim "opening mic" while a microphone is actually being opened.
       This read audio_live() alone, which goes false again the instant the
       recording stops -- so the label appeared at the END of an utterance,
       telling the user the mic was opening while the turn was being sent. */
    else if (ui_state == UI_LISTENING)
        txt = !audio_capturing()  ? "sending"
            : audio_live()        ? "listening"
                                  : "opening mic";
    else if (ts == TURN_SENDING)             txt = voice_transcript()[0] ? voice_transcript() : "sending";
    else if (ts == TURN_THINKING)            txt = "thinking";
    else if (voice_reply()[0])               txt = voice_reply();
    else if (voice_transcript()[0])          txt = voice_transcript();
    /* In hands free the instruction lives on the status line, and two
       different instructions on one screen is worse than one. */
    else if (net_voice_mode() == MODE_HANDSFREE) txt = "";
    else                                     txt = "hold to talk";
    if (strcmp(lv_label_get_text(face_text), txt) != 0)
        lv_label_set_text(face_text, txt);

    paint_status();
}

/* The face caption is one scrolling line. Tapping opens the whole answer as
   wrapped, scrollable text -- a voice device still has to be readable when the
   reply is longer than a glance. */
static void answer_open(void)
{
    if (answer_layer == NULL) return;
    const char *txt = voice_reply()[0] ? voice_reply()
                    : voice_transcript()[0] ? voice_transcript()
                    : "nothing yet";
    lv_label_set_text(answer_text, txt);
    lv_obj_scroll_to_y(answer_layer, 0, LV_ANIM_OFF);
    lv_obj_remove_flag(answer_layer, LV_OBJ_FLAG_HIDDEN);
    note_activity();
}

static bool answer_is_open(void)
{
    return answer_layer && !lv_obj_has_flag(answer_layer, LV_OBJ_FLAG_HIDDEN);
}

static void answer_close(void)
{
    if (answer_layer) lv_obj_add_flag(answer_layer, LV_OBJ_FLAG_HIDDEN);
}

/* Reports the PANEL, not the widget tree.
 *
 * The previous probe hung off the screen object, which never sees a click:
 * LVGL events do not bubble, so a full-screen layer catches every tap and the
 * screen handler stays silent whether touch works or not -- the two cases it
 * was built to tell apart. Reading the input device directly cannot be fooled
 * that way. */
static void touch_probe(void)
{
    static lv_indev_state_t prev = LV_INDEV_STATE_RELEASED;
    static bool warned;

    lv_indev_t *in = lv_indev_get_next(NULL);
    if (in == NULL) {
        if (!warned) { warned = true; ESP_LOGW(TAG, "touch: no input device at all"); }
        return;
    }
    const lv_indev_state_t st = lv_indev_get_state(in);
    if (st == prev) return;
    prev = st;
    lv_point_t p = {0, 0};
    lv_indev_get_point(in, &p);
    ESP_LOGI(TAG, "touch: %s at (%d,%d), page=%d",
             st == LV_INDEV_STATE_PRESSED ? "PRESS" : "release",
             (int)p.x, (int)p.y, (int)page);
}

/* Where the heat is going. Screen level dominates on an AMOLED, so say what it
   is and what is holding it there. */
static void health_log(void)
{
    /* Free heap is here because the audio peripheral is now created and
       destroyed once per turn, and a leak in that path would otherwise only
       show up as a mysterious failure many turns later. */
    ESP_LOGI(TAG, "health: screen %d%%, idle %u ms, mic %s, temp %.1fC, heap %u",
             screen_level, (unsigned)lv_tick_elaps(last_activity),
             audio_speech_started() ? "recording" :
             audio_capturing() ? "armed" : "shut",
             (double)last_temp_c, (unsigned)esp_get_free_heap_size());
}

/* Whether the microphone is open, said in words.
 *
 * This cannot live in the caption. The caption holds the last answer, and an
 * answer outlives the turn that produced it -- so the state line was being
 * hidden by text from a minute ago and the device looked identical whether it
 * was listening or not. Separate fact, separate line.
 *
 * Driven from audio_capturing()/audio_speech_started(), never from hf_on: the
 * switch being on is not the same claim as the microphone being open. */
static void paint_status(void)
{
    if (face_status == NULL) return;

    const turn_state_t ts = voice_state();
    const bool busy = (ts == TURN_SENDING || ts == TURN_THINKING || ts == TURN_SPEAKING);

    const char *txt;
    uint32_t    col;
    if (net_voice_mode() == MODE_REALTIME) {
        static char line[64];
        switch (realtime_state()) {
        case RT_OPENING:  txt = "CONNECTING";                col = 0xE0902F; break;
        case RT_LIVE:
            snprintf(line, sizeof line, "LIVE  %d:%02d  .  tap to end",
                     realtime_seconds() / 60, realtime_seconds() % 60);
            txt = line; col = 0x4FD69B; break;
        case RT_SPEAKING: txt = "SPEAKING";                  col = 0xE0565C; break;
        case RT_FAILED:   txt = realtime_error();            col = 0xE0565C; break;
        default:          txt = "TAP THE FACE TO CALL";      col = P.text;   break;
        }
    }
    else if (net_voice_mode() != MODE_HANDSFREE) { txt = ""; col = P.text; }
    /* The mic is deliberately shut while the device answers. Saying "opening
       the mic" through the whole reply would be the same lie in a new place. */
    else if (busy)                          { txt = ""; col = P.text; }
    else if (audio_speech_started())        { txt = "HEARING YOU";     col = 0xE0565C; }
    else if (audio_capturing())             { txt = "LISTENING  .  tap to stop"; col = 0x4FD69B; }
    else if (hf_on)                         { txt = "opening the mic"; col = 0xE0902F; }
    else                                    { txt = "TAP THE FACE TO LISTEN"; col = P.text; }

    if (strcmp(lv_label_get_text(face_status), txt) != 0)
        lv_label_set_text(face_status, txt);
    lv_obj_set_style_text_color(face_status, lv_color_hex(col), 0);
}

/* The caption is the reader: tapping it opens the whole answer. */
static void face_clicked(lv_event_t *e)
{
    LV_UNUSED(e);
    note_activity();
    if (answer_is_open()) answer_close();
    else                  answer_open();
}

/* The face itself is the switch.
 *
 * An always-open microphone should be something the owner turns on, not
 * something that follows from a setting they picked once. Hands free arms on a
 * tap and stops on a tap; the caption and the mic dot say which it is. */
static void face_tapped(lv_event_t *e)
{
    LV_UNUSED(e);
    note_activity();
    if (answer_is_open())                    { answer_close(); return; }

    /* Realtime FIRST. This branch used to sit below the line under it, which
       returns early for every mode that is not hands free -- so in realtime a
       tap opened the answer modal ("nothing yet" on an empty screen) and the
       call code was unreachable. One tap opens the call, the next ends it: a
       call holds a TLS session and the radio for its whole length, so it is
       started and ended deliberately, never drifted into. */
    if (net_voice_mode() == MODE_REALTIME) {
        if (realtime_state() == RT_OFF || realtime_state() == RT_FAILED)
            realtime_start();
        else
            realtime_stop();
        paint_status();
        return;
    }

    if (net_voice_mode() != MODE_HANDSFREE)  { answer_open();  return; }

    hf_on = !hf_on;
    if (!hf_on) {
        /* Stop means stop now. A turn already under way is submitted rather
           than thrown away -- the words were spoken, they should count. */
        if (ui_state == UI_LISTENING) set_state(UI_IDLE);
        else                          audio_disarm();
    }
    paint_status();          /* now, not on the next tick */
    ESP_LOGI(TAG, "touch: face tapped -> hands free %s",
             hf_on ? "listening" : "stopped");
}

static void build_answer_layer(lv_obj_t *parent)
{
    answer_layer = full_layer(parent);
    lv_obj_add_flag(answer_layer, LV_OBJ_FLAG_HIDDEN);
    themed(answer_layer, ROLE_BG, false);
    lv_obj_set_style_bg_opa(answer_layer, LV_OPA_COVER, 0);
    lv_obj_set_style_pad_all(answer_layer, 26, 0);
    lv_obj_set_scroll_dir(answer_layer, LV_DIR_VER);
    lv_obj_set_scrollbar_mode(answer_layer, LV_SCROLLBAR_MODE_AUTO);
    lv_obj_add_event_cb(answer_layer, face_clicked, LV_EVENT_CLICKED, NULL);

    answer_text = lv_label_create(answer_layer);
    themed(answer_text, ROLE_INK, true);
    lv_obj_set_style_text_font(answer_text, &lv_font_montserrat_24, 0);
    lv_label_set_long_mode(answer_text, LV_LABEL_LONG_WRAP);
    lv_obj_set_width(answer_text, SCR_W - 60);
    lv_label_set_text(answer_text, "");
    lv_obj_remove_flag(answer_text, LV_OBJ_FLAG_CLICKABLE);

    /* An exit that is visible, because a modal you cannot see how to leave is
       worse than no modal. */
    lv_obj_t *hint = lv_label_create(answer_layer);
    themed(hint, ROLE_TEXT, true);
    lv_obj_set_style_text_font(hint, &lv_font_montserrat_16, 0);
    lv_label_set_text(hint, "tap or press to close");
    lv_obj_remove_flag(hint, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_align(hint, LV_ALIGN_BOTTOM_MID, 0, 0);
}

static void build_face_layer(lv_obj_t *parent)
{
    face_layer = full_layer(parent);
    lv_obj_add_flag(face_layer, LV_OBJ_FLAG_HIDDEN);
    themed(face_layer, ROLE_BG, false);
    lv_obj_set_style_bg_opa(face_layer, LV_OPA_COVER, 0);

    brow_l = block(face_layer, BROW_W, BROW_H, ROLE_FACE);
    brow_r = block(face_layer, BROW_W, BROW_H, ROLE_FACE);
    lv_obj_set_pos(brow_l, SCR_W / 2 - EYE_DX - BROW_W / 2, EYE_Y - 34);
    lv_obj_set_pos(brow_r, SCR_W / 2 + EYE_DX - BROW_W / 2, EYE_Y - 34);

    eye_l = block(face_layer, EYE_W, EYE_OPEN, ROLE_FACE);
    eye_r = block(face_layer, EYE_W, EYE_OPEN, ROLE_FACE);
    lv_obj_set_pos(eye_l, SCR_W / 2 - EYE_DX - EYE_W / 2, EYE_Y);
    lv_obj_set_pos(eye_r, SCR_W / 2 + EYE_DX - EYE_W / 2, EYE_Y);

    const int mouth_top = MOUTH_Y + (MOUTH_MAX - MOUTH_MIN) / 2;
    /* One bright square in the upper-left of each eye. A pixel face reads as
       a machine without it and as a character with it, for two objects. */
    glint_l = block(eye_l, GLINT, GLINT, ROLE_GLINT);
    glint_r = block(eye_r, GLINT, GLINT, ROLE_GLINT);
    lv_obj_set_pos(glint_l, 12, 12);
    lv_obj_set_pos(glint_r, 12, 12);

    /* Blush. Soft, low, outboard of the eyes -- the oldest trick there is,
       and two objects. Kept translucent so it reads as colour on the cheek
       rather than another lit block. */
    blush_l = block(face_layer, BLUSH_W, BLUSH_H, ROLE_BLUSH);
    blush_r = block(face_layer, BLUSH_W, BLUSH_H, ROLE_BLUSH);
    lv_obj_set_style_bg_opa(blush_l, LV_OPA_40, 0);
    lv_obj_set_style_bg_opa(blush_r, LV_OPA_40, 0);
    lv_obj_set_pos(blush_l, SCR_W / 2 - EYE_DX - BLUSH_W / 2 - 16, EYE_Y + EYE_OPEN + 14);
    lv_obj_set_pos(blush_r, SCR_W / 2 + EYE_DX - BLUSH_W / 2 + 16, EYE_Y + EYE_OPEN + 14);

    mouth = block(face_layer, MOUTH_W, MOUTH_MIN, ROLE_FACE);
    lv_obj_set_pos(mouth, SCR_W / 2 - MOUTH_W / 2, mouth_top);

    corner_l = block(face_layer, CORNER_W, CORNER_H, ROLE_FACE);
    corner_r = block(face_layer, CORNER_W, CORNER_H, ROLE_FACE);
    lv_obj_set_pos(corner_l, SCR_W / 2 - MOUTH_W / 2 - CORNER_W + 8, mouth_top - SMILE_LIFT);
    lv_obj_set_pos(corner_r, SCR_W / 2 + MOUTH_W / 2 - 8,            mouth_top - SMILE_LIFT);

    /* Long transcripts scroll rather than truncate. */
    face_text = lv_label_create(face_layer);
    themed(face_text, ROLE_TEXT, true);
    lv_obj_set_style_text_font(face_text, &lv_font_montserrat_24, 0);
    lv_label_set_long_mode(face_text, LV_LABEL_LONG_SCROLL_CIRCULAR);
    lv_obj_set_width(face_text, SCR_W - 48);
    lv_obj_set_style_text_align(face_text, LV_TEXT_ALIGN_CENTER, 0);
    lv_label_set_text(face_text, "hold to talk");
    lv_obj_align(face_text, LV_ALIGN_BOTTOM_MID, 0, -34);

    face_status = lv_label_create(face_layer);
    lv_obj_set_style_text_font(face_status, &lv_font_montserrat_20, 0);
    lv_obj_set_style_text_align(face_status, LV_TEXT_ALIGN_CENTER, 0);
    lv_obj_remove_flag(face_status, LV_OBJ_FLAG_CLICKABLE);
    lv_label_set_text(face_status, "");
    lv_obj_align(face_status, LV_ALIGN_BOTTOM_MID, 0, -80);
    /* The scrolling caption is what invites the tap, so make it the target
       rather than relying on hitting bare background. */
    lv_obj_add_flag(face_text, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_add_event_cb(face_text, face_clicked, LV_EVENT_CLICKED, NULL);

    lv_obj_add_flag(face_layer, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_add_event_cb(face_layer, face_tapped, LV_EVENT_CLICKED, NULL);

    lv_timer_create(blink_tick, 260, NULL);
    lv_timer_create(face_tick, 80, NULL);
}

/* Voice mode, chosen by tapping.
 *
 * The three rows mirror the server's own AudioSurface taxonomy rather than
 * inventing a parallel one. Only dictate is built; the other two are shown
 * greyed with "not built yet" instead of being hidden -- a setting that
 * silently behaves like a different setting is worse than one that admits
 * what it cannot do. */
static void mode_paint(void)
{
    for (int i = 0; i < 3; i++) {
        if (mode_row[i] == NULL) continue;
        const bool sel   = ((int)net_voice_mode() == i);
        const bool avail = net_voice_mode_available((voice_mode_t)i);
        lv_obj_set_style_bg_color(mode_row[i],
            lv_color_hex(sel ? P.face : P.card), 0);
        lv_obj_set_style_bg_opa(mode_row[i], sel ? LV_OPA_COVER : LV_OPA_50, 0);
        lv_obj_set_style_border_color(mode_row[i], lv_color_hex(P.arena), 0);
        lv_obj_set_style_text_color(mode_lbl[i],
            lv_color_hex(sel ? P.bg : (avail ? P.ink : P.text)), 0);
        static const char *how[3] = {
            "hold to talk",
            "just talk",
            "live call",
        };
        char line[64];
        snprintf(line, sizeof line, "%s\n%s",
                 net_voice_mode_name((voice_mode_t)i),
                 avail ? how[i] : how[2]);
        lv_label_set_text(mode_lbl[i], line);
    }
    /* Say the cost out loud. An always-open microphone is a real trade, and
       a settings screen that hides it is selling the setting. */
    if (set_foot)
        lv_label_set_text(set_foot,
            net_voice_mode() == MODE_HANDSFREE
                ? "voice lives on the face page  .  tap it to listen"
                : "tap to choose  .  press to switch page");
}

static void mode_clicked(lv_event_t *e)
{
    const int idx = (int)(intptr_t)lv_event_get_user_data(e);
    note_activity();
    /* Refusing the tap is the honest response. Storing a mode the firmware
       cannot honour would leave the screen claiming one thing and the device
       doing another, which is the failure this project keeps guarding. */
    if (!net_voice_mode_available((voice_mode_t)idx)) return;
    net_set_voice_mode((voice_mode_t)idx);
    mode_paint();
}

static void build_settings_layer(lv_obj_t *parent)
{
    set_layer = full_layer(parent);
    lv_obj_add_flag(set_layer, LV_OBJ_FLAG_HIDDEN);
    themed(set_layer, ROLE_BG, false);
    lv_obj_set_style_bg_opa(set_layer, LV_OPA_COVER, 0);

    lv_obj_t *title = lv_label_create(set_layer);
    lv_obj_set_style_text_font(title, &lv_font_montserrat_28, 0);
    themed(title, ROLE_INK, true);
    lv_label_set_text(title, "voice mode");
    lv_obj_align(title, LV_ALIGN_TOP_MID, 0, 46);

    for (int i = 0; i < 3; i++) {
        mode_row[i] = lv_obj_create(set_layer);
        lv_obj_remove_style_all(mode_row[i]);
        lv_obj_set_size(mode_row[i], SCR_W - 76, 62);
        lv_obj_align(mode_row[i], LV_ALIGN_TOP_MID, 0, 118 + i * 76);
        lv_obj_set_style_radius(mode_row[i], 10, 0);
        lv_obj_set_style_border_width(mode_row[i], 1, 0);
        lv_obj_add_flag(mode_row[i], LV_OBJ_FLAG_CLICKABLE);
        lv_obj_add_event_cb(mode_row[i], mode_clicked, LV_EVENT_CLICKED,
                            (void *)(intptr_t)i);

        mode_lbl[i] = lv_label_create(mode_row[i]);
        lv_obj_set_style_text_font(mode_lbl[i], &lv_font_montserrat_20, 0);
        lv_obj_remove_flag(mode_lbl[i], LV_OBJ_FLAG_CLICKABLE);
        lv_obj_set_style_text_align(mode_lbl[i], LV_TEXT_ALIGN_CENTER, 0);
        lv_obj_center(mode_lbl[i]);
    }

    set_foot = lv_label_create(set_layer);
    lv_obj_set_style_text_font(set_foot, &lv_font_montserrat_16, 0);
    themed(set_foot, ROLE_TEXT, true);
    lv_obj_set_style_text_align(set_foot, LV_TEXT_ALIGN_CENTER, 0);
    lv_obj_align(set_foot, LV_ALIGN_BOTTOM_MID, 0, -14);

    mode_paint();
}

static void build_voice_layer(lv_obj_t *parent)
{
    voice_layer = full_layer(parent);
    lv_obj_add_flag(voice_layer, LV_OBJ_FLAG_HIDDEN);
    themed(voice_layer, ROLE_BG, false);
    lv_obj_set_style_bg_opa(voice_layer, LV_OPA_COVER, 0);

    lv_obj_t *halo = lv_obj_create(voice_layer);
    lv_obj_remove_style_all(halo);
    lv_obj_set_size(halo, 250, 250);
    lv_obj_center(halo);
    lv_obj_set_style_radius(halo, LV_RADIUS_CIRCLE, 0);
    themed(halo, ROLE_HALO, false);
    lv_obj_set_style_bg_opa(halo, LV_OPA_COVER, 0);

    voice_core = lv_obj_create(voice_layer);
    lv_obj_remove_style_all(voice_core);
    lv_obj_set_size(voice_core, 130, 130);
    lv_obj_center(voice_core);
    lv_obj_set_style_radius(voice_core, LV_RADIUS_CIRCLE, 0);
    themed(voice_core, ROLE_ORB, false);
    lv_obj_set_style_bg_opa(voice_core, LV_OPA_COVER, 0);
    lv_obj_set_style_bg_grad_color(voice_core, lv_color_hex(P.orbg), 0);
    lv_obj_set_style_bg_grad_dir(voice_core, LV_GRAD_DIR_VER, 0);

    lv_anim_t a;
    lv_anim_init(&a);
    lv_anim_set_var(&a, voice_core);
    lv_anim_set_exec_cb(&a, anim_size);
    lv_anim_set_values(&a, 118, 152);
    lv_anim_set_duration(&a, 900);
    lv_anim_set_playback_duration(&a, 900);
    lv_anim_set_repeat_count(&a, LV_ANIM_REPEAT_INFINITE);
    lv_anim_set_path_cb(&a, lv_anim_path_ease_in_out);
    lv_anim_start(&a);

    voice_lbl = lv_label_create(voice_layer);
    themed(voice_lbl, ROLE_INK, true);
    lv_obj_set_style_text_font(voice_lbl, &lv_font_montserrat_40, 0);
    lv_label_set_text(voice_lbl, "listening");
    lv_obj_align(voice_lbl, LV_ALIGN_BOTTOM_MID, 0, -56);
}

static void theme_tick(lv_timer_t *t)
{
    LV_UNUSED(t);
    static int last = -1;
    const int want = net_dark_theme() ? 1 : 0;
    if (want != last) {
        last = want;
        theme_apply(want != 0);      /* live: no reboot to change the look */
        ESP_LOGI(TAG, "theme applied: %s across %d objects",
                 want ? "dark" : "light", s_themed_n);
    }

    /* /config can change the mode too, and a settings screen that still shows
       the old selection is worse than no settings screen. */
    static int last_mode = -1;
    const int m = (int)net_voice_mode();
    if (m != last_mode) { last_mode = m; if (mode_row[0]) mode_paint(); }
}

/* One-shot dump of what is actually painted, with computed styles rather than
   what the code intended. Two rounds of "it should be dark" beat one round of
   reading the pixels back. */
static void dump_tree(lv_obj_t *o, int depth)
{
    const uint32_t n = lv_obj_get_child_count(o);
    for (uint32_t i = 0; i < n; i++) {
        lv_obj_t *c = lv_obj_get_child(o, i);
        const lv_color_t bg = lv_obj_get_style_bg_color(c, LV_PART_MAIN);
        const lv_opa_t   op = lv_obj_get_style_bg_opa(c, LV_PART_MAIN);
        const lv_color_t bd = lv_obj_get_style_border_color(c, LV_PART_MAIN);
        const int bw = lv_obj_get_style_border_width(c, LV_PART_MAIN);
        ESP_LOGI(TAG, "%*s[%u] %dx%d @%d,%d bg=%02X%02X%02X opa=%d border=%d/%02X%02X%02X%s",
                 depth * 2, "", (unsigned)i,
                 (int)lv_obj_get_width(c), (int)lv_obj_get_height(c),
                 (int)lv_obj_get_x(c), (int)lv_obj_get_y(c),
                 bg.red, bg.green, bg.blue, (int)op,
                 bw, bd.red, bd.green, bd.blue,
                 lv_obj_has_flag(c, LV_OBJ_FLAG_HIDDEN) ? " HIDDEN" : "");
        if (depth < 1) dump_tree(c, depth + 1);
    }
}

static void dump_once(lv_timer_t *t)
{
    lv_timer_delete(t);
    ESP_LOGI(TAG, "--- screen tree ---");
    dump_tree(lv_screen_active(), 0);
    ESP_LOGI(TAG, "--- end ---");
}

static void build_ui(void)
{
    P = net_dark_theme() ? PAL_DARK : PAL_LIGHT;
    ESP_LOGI(TAG, "theme at build: %s (bg %06X, ring %06X)",
             net_dark_theme() ? "dark" : "light", (unsigned)P.bg, (unsigned)P.ring);
    lv_obj_t *scr = lv_screen_active();
    lv_obj_remove_style_all(scr);
    lv_obj_set_style_bg_color(scr, lv_color_hex(P.bg), 0);
    lv_obj_set_style_bg_opa(scr, LV_OPA_COVER, 0);

    game_layer = full_layer(scr);

    lv_obj_t *arena = lv_obj_create(game_layer);
    lv_obj_remove_style_all(arena);
    lv_obj_set_size(arena, (int)(ARENA_R * 2), (int)(ARENA_R * 2));
    lv_obj_center(arena);
    lv_obj_set_style_radius(arena, LV_RADIUS_CIRCLE, 0);
    /* Outline, not a disc.
     *
     * This was a hardcoded 0xE8EDF8 fill -- a literal no palette referenced,
     * so every theme substitution passed it by and a 352px light circle sat on
     * a black ground regardless of the setting. On AMOLED it was also the
     * single largest lit area on screen, carrying no information: the boundary
     * is the point, not the interior. */
    lv_obj_set_style_bg_opa(arena, LV_OPA_TRANSP, 0);
    lv_obj_set_style_border_width(arena, 2, 0);
    lv_obj_set_style_border_color(arena, lv_color_hex(P.arena), 0);
    lv_obj_set_style_border_opa(arena, LV_OPA_COVER, 0);
    s_arena = arena;

    rings_generate();
    rings_draw();
    ESP_LOGI(TAG, "%d rings in a %d px arena; corridor %.1f px, ball %.0f px",
             RINGS, (int)ARENA_R,
             (145.0f - 40.0f) / (RINGS - 1) - RING_T - BALL_R * 2, BALL_R * 2);

    lv_obj_t *gring = lv_obj_create(game_layer);
    lv_obj_remove_style_all(gring);
    lv_obj_set_size(gring, (int)(GOAL_R * 2 + 10), (int)(GOAL_R * 2 + 10));
    lv_obj_set_pos(gring, (int)(goal_cx() - GOAL_R - 5), (int)(goal_cy() - GOAL_R - 5));
    lv_obj_set_style_radius(gring, LV_RADIUS_CIRCLE, 0);
    themed(gring, ROLE_GOALBG, false);
    lv_obj_set_style_bg_opa(gring, LV_OPA_COVER, 0);

    goal_obj = lv_obj_create(game_layer);
    lv_obj_remove_style_all(goal_obj);
    lv_obj_set_size(goal_obj, (int)(GOAL_R * 2), (int)(GOAL_R * 2));
    lv_obj_set_pos(goal_obj, (int)(goal_cx() - GOAL_R), (int)(goal_cy() - GOAL_R));
    lv_obj_set_style_radius(goal_obj, LV_RADIUS_CIRCLE, 0);
    themed(goal_obj, ROLE_GOAL, false);
    lv_obj_set_style_bg_opa(goal_obj, LV_OPA_COVER, 0);

    ball_obj = lv_obj_create(game_layer);
    lv_obj_remove_style_all(ball_obj);
    lv_obj_set_size(ball_obj, (int)(BALL_R * 2), (int)(BALL_R * 2));
    lv_obj_set_style_radius(ball_obj, LV_RADIUS_CIRCLE, 0);
    themed(ball_obj, ROLE_BALL, false);
    lv_obj_set_style_bg_opa(ball_obj, LV_OPA_COVER, 0);

    hint_lbl = lv_label_create(game_layer);
    themed(hint_lbl, ROLE_TEXT, true);
    lv_obj_set_style_text_font(hint_lbl, &lv_font_montserrat_20, 0);
    lv_label_set_text(hint_lbl, "tilt to the core  .  press to switch");
    lv_obj_align(hint_lbl, LV_ALIGN_BOTTOM_MID, 0, -14);

    build_face_layer(scr);
    build_answer_layer(scr);
    build_feed_layer(scr);
    build_settings_layer(scr);
    build_voice_layer(scr);

    /* Centred, not corner-pinned: this panel clipped a top-left label, and a
       centred one cannot be lost to a column offset. */
    /* Connection as a dot rather than a sentence: it is glanceable from any
       page and costs no room the content wants. */
    /* Says whether the MICROPHONE is open, which is a different fact from
       whether hands free is switched on -- the distinction this project has
       got wrong nine times. Driven from the capture state, never the setting. */
    mic_dot = lv_obj_create(scr);
    lv_obj_remove_style_all(mic_dot);
    lv_obj_set_size(mic_dot, 16, 16);
    lv_obj_set_style_radius(mic_dot, LV_RADIUS_CIRCLE, 0);
    lv_obj_set_style_bg_opa(mic_dot, LV_OPA_COVER, 0);
    lv_obj_add_flag(mic_dot, LV_OBJ_FLAG_HIDDEN);
    lv_obj_align(mic_dot, LV_ALIGN_TOP_MID, 74, 22);

    /* Every click, with where it landed and what caught it.
     *
     * Two rounds have now been spent unable to say whether a tap reaches the
     * handler or reaches it and produces nothing visible. Those look identical
     * from the outside and have completely different fixes. */


    net_dot = lv_obj_create(scr);
    lv_obj_remove_style_all(net_dot);
    lv_obj_set_size(net_dot, 16, 16);
    lv_obj_set_style_radius(net_dot, LV_RADIUS_CIRCLE, 0);
    lv_obj_set_style_bg_opa(net_dot, LV_OPA_COVER, 0);
    lv_obj_set_style_bg_color(net_dot, lv_color_hex(0xE0902F), 0);
    lv_obj_align(net_dot, LV_ALIGN_TOP_MID, -74, 22);

    temp_lbl = lv_label_create(scr);
    themed(temp_lbl, ROLE_TEXT, true);
    lv_obj_set_style_text_font(temp_lbl, &lv_font_montserrat_28, 0);
    lv_label_set_text(temp_lbl, "-- C");
    lv_obj_align(temp_lbl, LV_ALIGN_TOP_MID, 0, 14);
    lv_obj_move_foreground(temp_lbl);
    lv_obj_move_foreground(net_dot);
    lv_obj_move_foreground(mic_dot);

    reset_ball();
    lv_obj_set_pos(ball_obj, (int)(bx - BALL_R), (int)(by - BALL_R));

    const gpio_config_t btn = {
        .pin_bit_mask = 1ULL << BTN_GPIO, .mode = GPIO_MODE_INPUT,
        .pull_up_en = GPIO_PULLUP_ENABLE, .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type = GPIO_INTR_DISABLE,
    };
    gpio_config(&btn);

    lv_timer_create(tick, TICK_MS, NULL);
    lv_timer_create(temp_tick, 1000, NULL);
    lv_timer_create(theme_tick, 500, NULL);
    lv_timer_create(dump_once, 2500, NULL);
    lv_timer_create(feed_tick, 400, NULL);
}

static void imu_start(void)
{
    i2c_master_bus_handle_t bus = bsp_i2c_get_handle();
    if (bus == NULL) { ESP_LOGE(TAG, "no I2C bus"); return; }
    if (qmi8658_init(&imu, bus, BSP_IMU_I2C_ADDRESS) != ESP_OK) {
        ESP_LOGE(TAG, "QMI8658 init failed"); return;
    }
    uint8_t who = 0;
    qmi8658_get_who_am_i(&imu, &who);
    ESP_LOGI(TAG, "QMI8658 WHO_AM_I = 0x%02X", who);
    qmi8658_set_accel_range(&imu, QMI8658_ACCEL_RANGE_4G);
    qmi8658_set_accel_odr(&imu, QMI8658_ACCEL_ODR_250HZ);
    qmi8658_set_gyro_range(&imu, QMI8658_GYRO_RANGE_512DPS);
    qmi8658_set_gyro_odr(&imu, QMI8658_GYRO_ODR_250HZ);
    qmi8658_enable_accel(&imu, true);
    qmi8658_enable_gyro(&imu, true);
    imu.accel_unit_mps2 = true;
    imu.gyro_unit_rads  = false;
    imu_ok = true;

    last_activity = lv_tick_get();
    /* Level the board against however it is actually sitting. */
    float sx = 0, sy = 0;
    int n = 0;
    for (int i = 0; i < 60; i++) {
        float gx, gy, gz;
        if (qmi8658_read_accel(&imu, &gx, &gy, &gz) == ESP_OK) {
            sx += gx; sy += gy; n++;
        }
        vTaskDelay(pdMS_TO_TICKS(10));
    }
    if (n > 0) { bias_x = sx / n; bias_y = sy / n; }
    ESP_LOGI(TAG, "levelled: bias (%.2f, %.2f) from %d samples", bias_x, bias_y, n);
}

void app_main(void)
{
    ESP_LOGI(TAG, "board variant: %s", bsp_board_variant_to_name(bsp_board_detect()));
    lv_display_t *disp = bsp_display_start();
    if (disp == NULL) { ESP_LOGE(TAG, "display init FAILED"); return; }
    bsp_display_backlight_on();
    imu_start();
    audio_init();
    voice_init();
    net_start();
    if (bsp_display_lock(1000)) { build_ui(); bsp_display_unlock(); }
    else { ESP_LOGE(TAG, "LVGL lock failed"); return; }
    ESP_LOGI(TAG, "RINGS RUNNING  rings=%d imu=%d", RINGS, (int)imu_ok);
}
