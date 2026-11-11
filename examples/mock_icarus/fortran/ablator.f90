!> mock_icarus: a small 1D material-response code for a pyrolyzing ablator.
!>
!> This is NOT Icarus. It is a stand-in written to look like the kind of
!> legacy Fortran a GRASS user would want to drive: module state, its own
!> time integrator, and a flat C interface (iso_c_binding) so any host code
!> can call it. The physics is the textbook subset of what Icarus solves:
!>
!>   * transient conduction through a char/virgin mixture, k and cp blended
!>     by the local extent of decomposition beta (0 = virgin, 1 = char)
!>   * single-reaction Arrhenius pyrolysis, d(beta)/dt = A exp(-E/RT) (1-beta)
!>     with the heat of pyrolysis taken as a volumetric sink; the pyrolysis
!>     gas leaves through the heated surface instantly (no Darcy flow)
!>   * a surface energy balance with a heat-transfer coefficient
!>     ch = rho_e u_e C_H (kg/m2/s) and recovery enthalpy h_r supplied by the
!>     caller:  ch (h_r - h_w) - eps sigma (Tw^4 - T_inf^4) = q_cond
!>   * char recession from a diffusion-limited oxidation model,
!>     mdot_c = B'c(Tw) ch,  sdot = mdot_c / rho_char
!>
!> Each "station" is an independent 1D column (one point on the heatshield).
!> Fully implicit (backward Euler) in temperature with a linearised
!> re-radiation term, tridiagonal solve per column. Recession is handled by
!> contracting the column uniformly (a landing-frame shortcut; good enough
!> for a mock).
!>
!> The blowing correction to ch is deliberately NOT applied here: the caller
!> (an aeroheating code) is expected to supply an already blowing-corrected
!> ch. That makes the coupling genuinely two-way, which is the point of the
!> demo.
module mock_icarus
  use iso_c_binding
  implicit none
  private

  real(c_double), parameter :: sigma_sb = 5.670374419d-8

  type :: material_t
    real(c_double) :: rho_v = 280d0, rho_c = 230d0    ! kg/m3 (PICA-like)
    real(c_double) :: k_v = 0.20d0, k_c = 0.45d0       ! W/m/K
    real(c_double) :: cp_v = 1300d0, cp_c = 1600d0     ! J/kg/K
    real(c_double) :: eps_v = 0.85d0, eps_c = 0.90d0
    real(c_double) :: a_pyro = 2.0d4, e_over_r = 8.5d3 ! 1/s, K
    real(c_double) :: h_pyro = 1.0d6                   ! J/kg of gas
    real(c_double) :: bc_max = 0.17d0                  ! diffusion-limited B'c
    real(c_double) :: t_ox = 1300d0, dt_ox = 250d0     ! oxidation onset, K
    real(c_double) :: cp_gas_edge = 1150d0             ! wall-enthalpy cp, J/kg/K
  end type

  type :: station_t
    real(c_double), allocatable :: t(:), beta(:)
    real(c_double) :: thickness = 0, recession = 0
    real(c_double) :: ch = 0, h_r = 0, t_inf = 300d0
    real(c_double) :: mdot_g = 0, mdot_c = 0, q_conv = 0, q_rad = 0
  end type

  type :: instance_t
    logical :: live = .false.
    integer :: n = 0
    real(c_double) :: time = 0
    type(material_t) :: mat
    type(station_t), allocatable :: st(:)
  end type

  integer, parameter :: max_instances = 64
  type(instance_t), save, target :: pool(max_instances)

  ! number of doubles in one station's restart record (excluding profiles)
  integer, parameter :: n_scalars = 6

contains

  ! ---------------------------------------------------------------- lifecycle

  !> Create an instance with nstations columns of ncells cells each.
  !> Returns a handle > 0, or -1 if the pool is full / arguments are bad.
  integer(c_int) function icarus_create(nstations, ncells, thickness, t0) bind(C)
    integer(c_int), value :: nstations, ncells
    real(c_double), value :: thickness, t0
    integer :: h, s
    icarus_create = -1
    if (nstations < 1 .or. ncells < 3 .or. thickness <= 0) return
    do h = 1, max_instances
      if (.not. pool(h)%live) exit
    end do
    if (h > max_instances) return
    pool(h)%live = .true.
    pool(h)%n = ncells
    pool(h)%time = 0
    pool(h)%mat = material_t()
    allocate(pool(h)%st(nstations))
    do s = 1, nstations
      allocate(pool(h)%st(s)%t(ncells), pool(h)%st(s)%beta(ncells))
      pool(h)%st(s)%t = t0
      pool(h)%st(s)%beta = 0
      pool(h)%st(s)%thickness = thickness
      pool(h)%st(s)%t_inf = t0
    end do
    icarus_create = h
  end function

  subroutine icarus_destroy(h) bind(C)
    integer(c_int), value :: h
    if (.not. valid(h)) return
    deallocate(pool(h)%st)
    pool(h)%live = .false.
  end subroutine

  !> Override a material property by name. Returns 0 on success, 1 if unknown.
  integer(c_int) function icarus_set_material(h, name, value) bind(C)
    integer(c_int), value :: h
    character(kind=c_char), intent(in) :: name(*)
    real(c_double), value :: value
    character(len=32) :: key
    icarus_set_material = 1
    if (.not. valid(h)) return
    key = c_to_f(name)
    icarus_set_material = 0
    select case (trim(key))
    case ('rho_virgin');  pool(h)%mat%rho_v = value
    case ('rho_char');    pool(h)%mat%rho_c = value
    case ('k_virgin');    pool(h)%mat%k_v = value
    case ('k_char');      pool(h)%mat%k_c = value
    case ('cp_virgin');   pool(h)%mat%cp_v = value
    case ('cp_char');     pool(h)%mat%cp_c = value
    case ('emissivity');  pool(h)%mat%eps_v = value; pool(h)%mat%eps_c = value
    case ('a_pyro');      pool(h)%mat%a_pyro = value
    case ('e_over_r');    pool(h)%mat%e_over_r = value
    case ('h_pyro');      pool(h)%mat%h_pyro = value
    case ('bc_max');      pool(h)%mat%bc_max = value
    case default;         icarus_set_material = 1
    end select
  end function

  ! ---------------------------------------------------------------- boundary

  !> Surface boundary condition for one station (1-based): heat-transfer
  !> coefficient ch = rho_e u_e C_H [kg/m2/s] and recovery enthalpy [J/kg].
  subroutine icarus_set_bc(h, s, ch, h_r) bind(C)
    integer(c_int), value :: h, s
    real(c_double), value :: ch, h_r
    if (.not. valid_station(h, s)) return
    pool(h)%st(s)%ch = ch
    pool(h)%st(s)%h_r = h_r
  end subroutine

  ! ---------------------------------------------------------------- stepping

  !> Advance every station by dt seconds. Returns 0, or a nonzero code:
  !>   1 bad handle, 2 non-finite temperature, 3 burned through.
  integer(c_int) function icarus_step(h, dt) bind(C)
    integer(c_int), value :: h
    real(c_double), value :: dt
    integer :: s
    icarus_step = 1
    if (.not. valid(h)) return
    icarus_step = 0
    do s = 1, size(pool(h)%st)
      call step_station(pool(h)%mat, pool(h)%st(s), pool(h)%n, dt, icarus_step)
      if (icarus_step /= 0) return
    end do
    pool(h)%time = pool(h)%time + dt
  end function

  subroutine step_station(m, st, n, dt, ierr)
    type(material_t), intent(in) :: m
    type(station_t), intent(inout) :: st
    integer, intent(in) :: n
    real(c_double), intent(in) :: dt
    integer(c_int), intent(inout) :: ierr
    real(c_double) :: dz, rho(n), k(n), cp(n), src(n), told(n)
    real(c_double) :: a(n), b(n), c(n), d(n), kf
    real(c_double) :: rate, dbeta, tw, eps, hw, bc, sdot
    integer :: i

    dz = st%thickness / n

    ! 1. pyrolysis, exact for first-order kinetics over the step at T^n
    st%mdot_g = 0
    do i = 1, n
      rate = m%a_pyro * exp(-m%e_over_r / st%t(i))
      dbeta = (1 - st%beta(i)) * (1 - exp(-rate * dt))
      st%beta(i) = st%beta(i) + dbeta
      ! gas produced in this cell per unit area per second
      src(i) = (m%rho_v - m%rho_c) * dbeta / dt * dz
      st%mdot_g = st%mdot_g + src(i)
      src(i) = -m%h_pyro * src(i) / dz          ! W/m3 sink
    end do

    ! 2. properties
    rho = m%rho_v + (m%rho_c - m%rho_v) * st%beta
    k   = m%k_v   + (m%k_c   - m%k_v)   * st%beta
    cp  = m%cp_v  + (m%cp_c  - m%cp_v)  * st%beta

    ! 3. surface quantities at T^n (cell 1 is the heated surface)
    tw   = st%t(1)
    eps  = m%eps_v + (m%eps_c - m%eps_v) * st%beta(1)
    hw   = m%cp_gas_edge * tw
    bc   = m%bc_max * 0.5d0 * (1 + tanh((tw - m%t_ox) / m%dt_ox)) * st%beta(1)
    st%mdot_c = bc * st%ch
    sdot = st%mdot_c / m%rho_c

    ! 4. backward Euler conduction, linearised surface balance
    told = st%t
    do i = 1, n
      a(i) = 0; c(i) = 0
      b(i) = rho(i) * cp(i) * dz / dt
      d(i) = rho(i) * cp(i) * dz / dt * told(i) + src(i) * dz
      if (i > 1) then
        kf = 2 * k(i) * k(i-1) / (k(i) + k(i-1)) / dz
        a(i) = -kf; b(i) = b(i) + kf
      end if
      if (i < n) then
        kf = 2 * k(i) * k(i+1) / (k(i) + k(i+1)) / dz
        c(i) = -kf; b(i) = b(i) + kf
      end if
    end do
    ! surface: ch*(h_r - cp_e*T) - eps*sigma*(T^4 - Tinf^4), T^4 linearised
    b(1) = b(1) + st%ch * m%cp_gas_edge + 4 * eps * sigma_sb * tw**3
    d(1) = d(1) + st%ch * st%h_r + eps * sigma_sb * (3 * tw**4 + st%t_inf**4)
    ! back face adiabatic: nothing to add
    call tridiag(a, b, c, d, st%t, n)

    if (any(st%t /= st%t) .or. any(st%t > 1d5)) then
      ierr = 2; return
    end if

    tw = st%t(1)
    st%q_conv = st%ch * (st%h_r - m%cp_gas_edge * tw)
    st%q_rad  = eps * sigma_sb * (tw**4 - st%t_inf**4)

    ! 5. recession: contract the column
    st%recession = st%recession + sdot * dt
    st%thickness = st%thickness - sdot * dt
    if (st%thickness < 2 * dz * 0.1d0) ierr = 3
  end subroutine

  subroutine tridiag(a, b, c, d, x, n)
    integer, intent(in) :: n
    real(c_double), intent(in) :: a(n), b(n), c(n), d(n)
    real(c_double), intent(out) :: x(n)
    real(c_double) :: cp(n), dp(n), m
    integer :: i
    cp(1) = c(1) / b(1); dp(1) = d(1) / b(1)
    do i = 2, n
      m = b(i) - a(i) * cp(i-1)
      cp(i) = c(i) / m
      dp(i) = (d(i) - a(i) * dp(i-1)) / m
    end do
    x(n) = dp(n)
    do i = n - 1, 1, -1
      x(i) = dp(i) - cp(i) * x(i+1)
    end do
  end subroutine

  ! ---------------------------------------------------------------- outputs

  !> Surface outputs for one station, written to out(1:8):
  !>   T_wall, T_back, recession, mdot_gas, mdot_char, q_conv, q_rad, thickness
  subroutine icarus_get(h, s, out) bind(C)
    integer(c_int), value :: h, s
    real(c_double), intent(out) :: out(8)
    out = 0
    if (.not. valid_station(h, s)) return
    associate (st => pool(h)%st(s))
      out = [st%t(1), st%t(pool(h)%n), st%recession, st%mdot_g, st%mdot_c, &
             st%q_conv, st%q_rad, st%thickness]
    end associate
  end subroutine

  !> Temperature and decomposition profiles (surface first), n values each.
  subroutine icarus_get_profile(h, s, t, beta) bind(C)
    integer(c_int), value :: h, s
    real(c_double), intent(out) :: t(*), beta(*)
    integer :: n
    if (.not. valid_station(h, s)) return
    n = pool(h)%n
    t(1:n) = pool(h)%st(s)%t
    beta(1:n) = pool(h)%st(s)%beta
  end subroutine

  integer(c_int) function icarus_ncells(h) bind(C)
    integer(c_int), value :: h
    icarus_ncells = 0
    if (valid(h)) icarus_ncells = pool(h)%n
  end function

  real(c_double) function icarus_time(h) bind(C)
    integer(c_int), value :: h
    icarus_time = 0
    if (valid(h)) icarus_time = pool(h)%time
  end function

  ! ---------------------------------------------------------------- restart

  !> Number of doubles icarus_save_state writes.
  integer(c_int) function icarus_state_size(h) bind(C)
    integer(c_int), value :: h
    icarus_state_size = 0
    if (valid(h)) icarus_state_size = 1 + size(pool(h)%st) * (n_scalars + 2 * pool(h)%n)
  end function

  subroutine icarus_save_state(h, buf) bind(C)
    integer(c_int), value :: h
    real(c_double), intent(out) :: buf(*)
    integer :: s, p, n
    if (.not. valid(h)) return
    n = pool(h)%n
    buf(1) = pool(h)%time
    p = 2
    do s = 1, size(pool(h)%st)
      associate (st => pool(h)%st(s))
        buf(p:p+n_scalars-1) = [st%thickness, st%recession, st%ch, st%h_r, st%mdot_g, st%mdot_c]
        p = p + n_scalars
        buf(p:p+n-1) = st%t; p = p + n
        buf(p:p+n-1) = st%beta; p = p + n
      end associate
    end do
  end subroutine

  subroutine icarus_restore_state(h, buf) bind(C)
    integer(c_int), value :: h
    real(c_double), intent(in) :: buf(*)
    integer :: s, p, n
    if (.not. valid(h)) return
    n = pool(h)%n
    pool(h)%time = buf(1)
    p = 2
    do s = 1, size(pool(h)%st)
      associate (st => pool(h)%st(s))
        st%thickness = buf(p); st%recession = buf(p+1); st%ch = buf(p+2)
        st%h_r = buf(p+3); st%mdot_g = buf(p+4); st%mdot_c = buf(p+5)
        p = p + n_scalars
        st%t = buf(p:p+n-1); p = p + n
        st%beta = buf(p:p+n-1); p = p + n
      end associate
    end do
  end subroutine

  ! ---------------------------------------------------------------- helpers

  logical function valid(h)
    integer(c_int), intent(in) :: h
    valid = h >= 1 .and. h <= max_instances
    if (valid) valid = pool(h)%live
  end function

  logical function valid_station(h, s)
    integer(c_int), intent(in) :: h, s
    valid_station = valid(h)
    if (valid_station) valid_station = s >= 1 .and. s <= size(pool(h)%st)
  end function

  function c_to_f(cs) result(fs)
    character(kind=c_char), intent(in) :: cs(*)
    character(len=32) :: fs
    integer :: i
    fs = ' '
    do i = 1, 32
      if (cs(i) == c_null_char) exit
      fs(i:i) = cs(i)
    end do
  end function

end module mock_icarus
