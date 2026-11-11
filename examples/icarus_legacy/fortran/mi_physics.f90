!=======================================================================
!  mi_physics: one time step of the material response.
!
!  Per step, Picard iterations on the coupled nonlinearities:
!    1. decomposition of the three components at the current T iterate
!       (exact integral of the n-th order Arrhenius law over the step)
!    2. pyrolysis-gas pressure from Darcy flow (implicit, BiCGSTAB)
!    3. energy with conduction, upwinded gas convection, the heat of
!       pyrolysis, and a linearised B' surface energy balance (BiCGSTAB)
!  then the step is accepted or rejected; on acceptance the surface
!  recedes and the columns contract.
!=======================================================================
module mi_physics
  use mi_globals
  use mi_parallel, only: halo, gmax, mi_fatal
  use mi_solver, only: bicgstab
  use mi_input, only: env_lookup, bprime_lookup
  use mi_mesh, only: update_geometry
  implicit none
  private
  public :: try_step, surface_balance, set_table_environment, init_fields, rho_virgin, rho_char

  integer, parameter :: STEP_OK = 0, STEP_REJECT = 1, STEP_BURN_THROUGH = 3

contains

  real(rk) function rho_virgin()
    rho_virgin = gamma_resin * (rho_vi(1) + rho_vi(2)) + (1 - gamma_resin) * rho_vi(3)
  end function

  real(rk) function rho_char()
    rho_char = gamma_resin * (rho_ci(1) + rho_ci(2)) + (1 - gamma_resin) * rho_ci(3)
  end function

  subroutine init_fields()
    integer :: c
    temp = t_init
    pres = p_init
    do c = 1, n_own
      rho_i(:, c) = rho_vi
    end do
    rho_s = rho_virgin()
    s_tw = t_init; s_mg = 0; s_mc = 0; s_rec = 0; s_qcond = 0; s_bc = 0
    s_ch0 = 0; s_ch = 0; s_hr = 0; s_pe = p_init; s_qrad = 0
    f_mdot = 0
  end subroutine

  !> Surface inputs from the environment table at time t.
  subroutine set_table_environment(t)
    real(rk), intent(in) :: t
    real(rk) :: e(4)
    integer :: s
    do s = 1, ns
      call env_lookup(s_theta(s), t, e)
      s_ch0(s) = e(1); s_hr(s) = e(2); s_pe(s) = e(3); s_qrad(s) = e(4)
    end do
  end subroutine

  !> Net heat flux into the wall q(T_w) from the B' energy balance, and the
  !> effective (blowing-corrected) ch, char mass flux and B'c it implies.
  subroutine surface_balance(s, tw, mg, eps, q, ch, mc, bc)
    integer, intent(in) :: s
    real(rk), intent(in) :: tw, mg, eps
    real(rk), intent(out) :: q, ch, mc, bc
    real(rk) :: ch0, hw, bg, phi, chn, hc, hg
    integer :: it
    ch0 = s_ch0(s)
    ch = ch0
    if (ch0 <= 0) then
      ch = 0; bc = 0; mc = 0
      call bprime_lookup(tw, 0d0, s_pe(s), bc, hw)
      bc = 0
    else if (blowing_correction) then
      do it = 1, 60
        bg = mg / ch
        call bprime_lookup(tw, bg, s_pe(s), bc, hw)
        phi = 2 * lambda_blow * (mg + bc * ch) / ch0
        if (phi < 1d-8) then
          chn = ch0
        else
          chn = ch0 * phi / (exp(phi) - 1)
        end if
        if (abs(chn - ch) < 1d-12 * ch0) then
          ch = chn; exit
        end if
        ch = 0.5d0 * (ch + chn)
      end do
      bg = mg / ch
      call bprime_lookup(tw, bg, s_pe(s), bc, hw)
    else
      bg = mg / ch
      call bprime_lookup(tw, bg, s_pe(s), bc, hw)
    end if
    mc = bc * ch
    hc = cp_c * (tw - t_ref)
    hg = h_gas0 + cp_gas * (tw - t_ref)
    q = ch * (s_hr(s) - hw) + mc * hc + mg * hg - (mg + mc) * hw &
      + absorptivity * s_qrad(s) - eps * sigma_sb * (tw**4 - t_inf**4)
  end subroutine

  !> Take one step of length h. Returns STEP_OK, STEP_REJECT (state
  !> unchanged; caller cuts the step) or STEP_BURN_THROUGH.
  integer function try_step(h, picard_its, lin_its) result(status)
    real(rk), intent(in) :: h
    integer, intent(out) :: picard_its, lin_its
    real(rk), allocatable :: t0(:), p0(:), rs0(:), ri0(:,:), tprev(:)
    real(rk), allocatable :: beta(:), kc(:), cpc(:), poro(:), perm(:), rhog(:), src(:)
    real(rk), allocatable :: diag(:), a_on(:), a_no(:), rhs(:), fc(:)
    real(rk), allocatable :: tw(:), mg(:), sa(:), sb(:), sg(:)
    real(rk) :: rv, rc, x0, x, kr, kf, cond, coef, cap, q, q2, ch, mc, bc, eps, dmax, dq, sdot
    integer :: c, f, o, n, s, m, i, its
    logical :: converged

    allocate(t0(n_tot), p0(n_tot), rs0(n_tot), ri0(3, n_own), tprev(n_tot))
    allocate(beta(n_tot), kc(n_tot), cpc(n_tot), poro(n_tot), perm(n_tot), rhog(n_tot), src(n_own))
    allocate(diag(n_own), a_on(nf), a_no(nf), rhs(n_own), fc(nf))
    allocate(tw(ns), mg(ns), sa(ns), sb(ns), sg(ns))
    t0 = temp; p0 = pres; rs0 = rho_s; ri0 = rho_i
    tw = s_tw; mg = s_mg
    rv = rho_virgin(); rc = rho_char()
    picard_its = 0; lin_its = 0
    converged = .false.

    do m = 1, max_picard
      picard_its = m
      tprev = temp

      ! 1. decomposition at the current temperature iterate
      do c = 1, n_own
        do i = 1, 3
          if (a_pre(i) <= 0 .or. rho_vi(i) <= rho_ci(i)) then
            rho_i(i, c) = ri0(i, c); cycle
          end if
          x0 = (ri0(i, c) - rho_ci(i)) / rho_vi(i)
          kr = a_pre(i) * exp(-e_over_r(i) / temp(c)) * h
          if (x0 <= 1d-300) then
            x = 0
          else if (abs(n_order(i) - 1) < 1d-12) then
            x = x0 * exp(-kr)
          else
            x = (x0**(1 - n_order(i)) + (n_order(i) - 1) * kr)**(1 / (1 - n_order(i)))
          end if
          rho_i(i, c) = rho_ci(i) + rho_vi(i) * x
        end do
        rho_s(c) = gamma_resin * (rho_i(1, c) + rho_i(2, c)) + (1 - gamma_resin) * rho_i(3, c)
        src(c) = (rs0(c) - rho_s(c)) / h          ! kg/m3/s of gas
      end do
      call halo(rho_s)

      ! 2. properties everywhere (ghosts included)
      do c = 1, n_tot
        beta(c) = min(1d0, max(0d0, (rv - rho_s(c)) / (rv - rc)))
        kc(c) = (1 - beta(c)) * (k_v0 + k_v1 * temp(c)) &
              + beta(c) * (k_c0 + k_c1 * temp(c) + k_rad * temp(c)**3)
        cpc(c) = (1 - beta(c)) * cp_v + beta(c) * cp_c
        poro(c) = (1 - beta(c)) * phi_v + beta(c) * phi_c
        perm(c) = (1 - beta(c)) * perm_v + beta(c) * perm_c
        rhog(c) = pres(c) * mw_gas / (r_univ * temp(c))
      end do

      ! 3. Darcy pressure:  phi M/(R T) dp/dt - div(rho_g K/mu grad p) = pi
      !    With rho_g = p M/(R T) the face flux is c (p_o^2 - p_n^2); it is
      !    linearised about the current iterate (one Newton step per Picard
      !    iteration), so the pressure converges with the temperature.
      do c = 1, n_own
        cap = poro(c) * mw_gas / (r_univ * temp(c)) * c_vol(c) / h
        diag(c) = cap
        rhs(c) = cap * p0(c) + src(c) * c_vol(c)
      end do
      a_on = 0; a_no = 0
      do f = 1, nf
        o = f_o(f); n = f_n(f)
        kf = 2 * perm(o) * perm(n) / (perm(o) + perm(n))
        fc(f) = mw_gas * kf / (mu_gas * r_univ * (temp(o) + temp(n))) * f_area(f) / f_dist(f)
        coef = fc(f) * (pres(o)**2 - pres(n)**2)
        diag(o) = diag(o) + 2 * fc(f) * pres(o); a_on(f) = -2 * fc(f) * pres(n)
        rhs(o) = rhs(o) + coef
        if (n <= n_own) then
          diag(n) = diag(n) + 2 * fc(f) * pres(n); a_no(f) = -2 * fc(f) * pres(o)
          rhs(n) = rhs(n) - coef
        end if
      end do
      do s = 1, ns
        c = s_cell(s)
        sg(s) = mw_gas * perm(c) / (2 * mu_gas * r_univ * temp(c)) * s_area(s) / s_dhalf(s)
        diag(c) = diag(c) + 2 * sg(s) * pres(c)
        rhs(c) = rhs(c) + sg(s) * (pres(c)**2 + s_pe(s)**2)
      end do
      its = bicgstab(diag, a_on, a_no, rhs, pres)
      if (its < 0) exit
      lin_its = lin_its + its
      pres = max(pres, 1d0)
      do f = 1, nf
        o = f_o(f); n = f_n(f)
        f_mdot(f) = fc(f) * (pres(o)**2 - pres(n)**2)     ! kg/s from o to n
      end do
      do s = 1, ns
        c = s_cell(s)
        mg(s) = max(0d0, sg(s) * (pres(c)**2 - s_pe(s)**2) / s_area(s))
      end do

      ! 4. energy
      do c = 1, n_own
        cap = rho_s(c) * cpc(c) * c_vol(c) / h
        diag(c) = cap
        rhs(c) = cap * t0(c) - h_pyro * src(c) * c_vol(c)
      end do
      a_on = 0; a_no = 0
      do f = 1, nf
        o = f_o(f); n = f_n(f)
        kf = 2 * kc(o) * kc(n) / (kc(o) + kc(n))
        cond = kf * f_area(f) / f_dist(f)
        diag(o) = diag(o) + cond; a_on(f) = -cond
        if (n <= n_own) then
          diag(n) = diag(n) + cond; a_no(f) = -cond
        end if
        ! upwind gas convection (non-conservative form): only the receiving cell
        if (f_mdot(f) > 0) then
          if (n <= n_own) then
            diag(n) = diag(n) + cp_gas * f_mdot(f)
            a_no(f) = a_no(f) - cp_gas * f_mdot(f)
          end if
        else
          diag(o) = diag(o) - cp_gas * f_mdot(f)
          a_on(f) = a_on(f) + cp_gas * f_mdot(f)
        end if
      end do
      do s = 1, ns
        c = s_cell(s)
        eps = (1 - beta(c)) * eps_v + beta(c) * eps_c
        call surface_balance(s, tw(s), mg(s), eps, q, ch, mc, bc)
        dq = max(1d-3, 1d-6 * tw(s))
        call surface_balance(s, tw(s) + dq, mg(s), eps, q2, ch, mc, bc)
        sb(s) = max(0d0, -(q2 - q) / dq)            ! -dq/dTw
        sa(s) = q + sb(s) * tw(s)                   ! q(Tw) ~ a - b Tw
        sg(s) = kc(c) / s_dhalf(s)
        diag(c) = diag(c) + s_area(s) * sg(s) * sb(s) / (sb(s) + sg(s))
        rhs(c) = rhs(c) + s_area(s) * sg(s) * sa(s) / (sb(s) + sg(s))
      end do
      its = bicgstab(diag, a_on, a_no, rhs, temp)
      if (its < 0) exit
      lin_its = lin_its + its
      if (gmax(merge(1d0, 0d0, any(temp(1:n_own) /= temp(1:n_own)))) > 0) exit
      do s = 1, ns
        tw(s) = (sa(s) + sg(s) * temp(s_cell(s))) / (sb(s) + sg(s))
      end do

      dmax = gmax(maxval(abs(temp(1:n_own) - tprev(1:n_own))))
      if (dmax < picard_tol) then
        converged = .true.
        exit
      end if
    end do

    ! every exit from the loop above is collective, so all ranks agree on
    ! `converged`; the size of the change is checked globally too
    dmax = gmax(maxval(abs(temp(1:n_own) - t0(1:n_own))))
    if (.not. (dmax <= dtemp_max)) converged = .false.
    if (.not. converged) then
      temp = t0; pres = p0; rho_s = rs0; rho_i = ri0
      status = STEP_REJECT
      return
    end if

    ! accept: surface state, recession, mesh motion
    status = STEP_OK
    do s = 1, ns
      c = s_cell(s)
      eps = (1 - beta(c)) * eps_v + beta(c) * eps_c
      s_tw(s) = tw(s); s_mg(s) = mg(s)
      call surface_balance(s, tw(s), mg(s), eps, q, ch, mc, bc)
      s_ch(s) = ch; s_mc(s) = mc; s_bc(s) = bc; s_qcond(s) = q
      sdot = mc / rho_s(c)
      s_rec(s) = s_rec(s) + sdot * h
      do i = 0, n_depth - 1
        r_out(c + i) = r_out(c + i) - sdot * h
      end do
      if (r_out(c) - r_inner < 0.1d0 * thickness) status = STEP_BURN_THROUGH
    end do
    call update_geometry()
    if (gmax(real(status, rk)) >= STEP_BURN_THROUGH) status = STEP_BURN_THROUGH
  end function

end module mi_physics
