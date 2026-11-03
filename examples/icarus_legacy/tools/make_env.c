/* make_env: the "run the flow code along the trajectory, hand the material
 * response a table" step of the classic one-way workflow.
 *
 * Flies the mock aeroheating code (../mock_icarus/c/aero.c) with no wall
 * blowing and writes the environment micarus reads in bc_source = 'table'
 * mode: ch0, h_r, p_e and q_rad vs. time at a few body stations.
 *
 *   cc -O2 -o make_env tools/make_env.c ../mock_icarus/c/aero.c -lm
 *   ./make_env > cases/env.dat
 */
#include <math.h>
#include <stdio.h>

int aero_create(int n, const double *theta_deg, double rn, double beta, double v0,
                double gamma0_deg, double h0);
int aero_step(int h, double dt);
void aero_get(int h, int s, double out[4]);
void aero_trajectory(int h, double out[5]);

/* Newtonian surface pressure plus the free-stream static pressure. Also
 * used by the GRASS adapter, so both workflows see the same p_e. */
double env_pressure(double rho, double v, double theta_deg) {
    double c = cos(theta_deg * M_PI / 180.0);
    return rho * v * v * c * c + rho * 287.0 * 250.0 + 10.0;
}

int main(void) {
    enum { NS = 7 };
    const double theta[NS] = {0, 10, 20, 30, 40, 50, 60};
    const double dt = 0.1, every = 0.5, t_end = 80.0;
    int h = aero_create(NS, theta, 0.22, 60.0, 11500.0, -8.0, 125000.0);
    int nrec = (int)(t_end / every + 0.5) + 1;
    printf("# micarus environment table (one-way, no blowing): from mock aero.c\n");
    printf("# nstations ntimes\n%d %d\n# station body angles [deg]\n", NS, nrec);
    for (int s = 0; s < NS; s++) printf("%g ", theta[s]);
    printf("\n# time [s], then per station: ch0 [kg/m2/s] h_r [J/kg] p_e [Pa] q_rad [W/m2]\n");
    int steps_per = (int)(every / dt + 0.5);
    for (int r = 0; r < nrec; r++) {
        double traj[5], e[4];
        aero_trajectory(h, traj);
        printf("%.4f", r * every);
        for (int s = 0; s < NS; s++) {
            aero_get(h, s, e);
            printf(" %.10e %.10e %.10e %.1f", e[2], e[1], env_pressure(traj[4], traj[2], theta[s]), 0.0);
        }
        printf("\n");
        for (int k = 0; k < steps_per; k++) aero_step(h, dt);
    }
    return 0;
}
