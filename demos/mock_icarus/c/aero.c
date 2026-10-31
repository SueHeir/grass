/* mock_aero: a stand-in for an aerothermal code (think DPLR/LAURA output
 * reduced to engineering correlations), written in plain C with a flat API.
 *
 *  - 3-DOF planar ballistic entry into an exponential Earth atmosphere,
 *    integrated with RK4 (the "flow solver" time step)
 *  - stagnation-point cold-wall heating from Sutton-Graves,
 *      q_cw = k sqrt(rho / Rn) V^3,  k = 1.7415e-4 (SI)
 *  - a fit to the laminar heating distribution over a spherical nose,
 *      q(theta)/q0 = 0.55 + 0.45 cos(2 theta)     (theta <= ~60 deg)
 *  - recovery enthalpy h_r = V^2/2 + cp T_inf
 *  - the heat-transfer coefficient ch0 = q_cw / (h_r - h_w,cold) and the
 *    laminar blowing correction
 *      ch = ch0 * phi / (exp(phi) - 1),  phi = 2 lambda mdot / ch0,  lambda = 0.5
 *    where mdot is the wall mass injection (pyrolysis gas + char) supplied
 *    by the material-response code. This is the two-way part.
 */
#include <math.h>
#include <stdlib.h>
#include <string.h>

#define MAX_INST 64
#define MAX_ST 64

typedef struct {
    int live, n;
    double theta[MAX_ST], mdot[MAX_ST];
    double rn, beta;              /* nose radius m, ballistic coefficient kg/m2 */
    double t, h, v, gamma, range; /* state */
} aero_t;

static aero_t pool[MAX_INST];

static const double RE = 6.371e6, G0 = 9.80665, RHO0 = 1.225, HS = 7200.0;
static const double K_SG = 1.7415e-4, CP_AIR = 1004.5, T_INF = 250.0, T_COLD = 300.0;

static aero_t *get(int h) { return (h >= 1 && h <= MAX_INST && pool[h - 1].live) ? &pool[h - 1] : NULL; }
static double density(double alt) { return RHO0 * exp(-alt / HS); }

int aero_create(int nstations, const double *theta_deg, double nose_radius, double ballistic_coef,
                double v0, double gamma0_deg, double h0) {
    if (nstations < 1 || nstations > MAX_ST) return -1;
    for (int i = 0; i < MAX_INST; i++) {
        if (pool[i].live) continue;
        aero_t *a = &pool[i];
        memset(a, 0, sizeof *a);
        a->live = 1; a->n = nstations;
        for (int s = 0; s < nstations; s++) a->theta[s] = theta_deg[s] * M_PI / 180.0;
        a->rn = nose_radius; a->beta = ballistic_coef;
        a->v = v0; a->gamma = gamma0_deg * M_PI / 180.0; a->h = h0;
        return i + 1;
    }
    return -1;
}

void aero_destroy(int h) { aero_t *a = get(h); if (a) a->live = 0; }

/* wall mass injection rate [kg/m2/s] at station s (0-based) */
void aero_set_blowing(int h, int s, double mdot) {
    aero_t *a = get(h);
    if (a && s >= 0 && s < a->n) a->mdot[s] = mdot > 0 ? mdot : 0;
}

static void rhs(const aero_t *a, const double y[4], double dy[4]) {
    /* y = h, v, gamma, range */
    double g = G0 * pow(RE / (RE + y[0]), 2), r = RE + y[0];
    double drag = density(y[0]) * y[1] * y[1] / (2 * a->beta);
    dy[0] = y[1] * sin(y[2]);
    dy[1] = -drag - g * sin(y[2]);
    dy[2] = (y[1] / r - g / y[1]) * cos(y[2]);
    dy[3] = y[1] * cos(y[2]) * RE / r;
}

/* Advance the trajectory by dt. Returns 0, or 1 when the vehicle is below
 * 5 km or slower than 300 m/s (end of the heating pulse). */
int aero_step(int h, double dt) {
    aero_t *a = get(h);
    if (!a) return 2;
    double y[4] = {a->h, a->v, a->gamma, a->range}, k1[4], k2[4], k3[4], k4[4], t[4];
    rhs(a, y, k1);
    for (int i = 0; i < 4; i++) t[i] = y[i] + 0.5 * dt * k1[i];
    rhs(a, t, k2);
    for (int i = 0; i < 4; i++) t[i] = y[i] + 0.5 * dt * k2[i];
    rhs(a, t, k3);
    for (int i = 0; i < 4; i++) t[i] = y[i] + dt * k3[i];
    rhs(a, t, k4);
    for (int i = 0; i < 4; i++) y[i] += dt / 6 * (k1[i] + 2 * k2[i] + 2 * k3[i] + k4[i]);
    a->h = y[0]; a->v = y[1]; a->gamma = y[2]; a->range = y[3]; a->t += dt;
    return (a->h < 5e3 || a->v < 300) ? 1 : 0;
}

/* Surface environment at station s (0-based): out = {ch, h_r, ch0, q_cw} */
void aero_get(int h, int s, double out[4]) {
    aero_t *a = get(h);
    memset(out, 0, 4 * sizeof(double));
    if (!a || s < 0 || s >= a->n) return;
    double q0 = K_SG * sqrt(density(a->h) / a->rn) * pow(a->v, 3);
    double q = q0 * (0.55 + 0.45 * cos(2 * a->theta[s]));
    double hr = 0.5 * a->v * a->v + CP_AIR * T_INF;
    double ch0 = q / (hr - CP_AIR * T_COLD);
    double phi = ch0 > 0 ? 2 * 0.5 * a->mdot[s] / ch0 : 0;
    double omega = phi > 1e-8 ? phi / expm1(phi) : 1.0;
    out[0] = ch0 * omega; out[1] = hr; out[2] = ch0; out[3] = q;
}

/* out = {t, altitude, velocity, flight-path angle (deg), density} */
void aero_trajectory(int h, double out[5]) {
    aero_t *a = get(h);
    memset(out, 0, 5 * sizeof(double));
    if (!a) return;
    out[0] = a->t; out[1] = a->h; out[2] = a->v; out[3] = a->gamma * 180 / M_PI; out[4] = density(a->h);
}

int aero_state_size(int h) { aero_t *a = get(h); return a ? 5 + a->n : 0; }
void aero_save_state(int h, double *buf) {
    aero_t *a = get(h);
    if (!a) return;
    buf[0] = a->t; buf[1] = a->h; buf[2] = a->v; buf[3] = a->gamma; buf[4] = a->range;
    memcpy(buf + 5, a->mdot, a->n * sizeof(double));
}
void aero_restore_state(int h, const double *buf) {
    aero_t *a = get(h);
    if (!a) return;
    a->t = buf[0]; a->h = buf[1]; a->v = buf[2]; a->gamma = buf[3]; a->range = buf[4];
    memcpy(a->mdot, buf + 5, a->n * sizeof(double));
}
