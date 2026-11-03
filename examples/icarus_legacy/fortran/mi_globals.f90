!=======================================================================
!  micarus -- a mock entry-systems material-response code
!
!  mi_globals: every piece of program state, as module variables.
!
!  This is deliberately written the way large legacy Fortran codes are:
!  one "common block" of module data that every routine reaches into,
!  namelist-driven inputs with defaults declared next to the variables,
!  and fixed-size tables for small lists. There is exactly one instance
!  of the solver per process.
!
!  NOT Icarus. Written from public descriptions of 3D unstructured
!  finite-volume material-response codes (Darcy flow of pyrolysis gas,
!  multi-component Arrhenius decomposition, tabulated surface
!  thermochemistry, mesh motion for recession). Physics and numbers are
!  plausible, not validated.
!=======================================================================
module mi_globals
  use mpi
  implicit none
  public

  integer, parameter :: rk = kind(1.0d0)
  real(rk), parameter :: pi = 3.14159265358979323846d0
  real(rk), parameter :: sigma_sb = 5.670374419d-8
  real(rk), parameter :: r_univ = 8.314462618d0
  real(rk), parameter :: t_ref = 298.15d0

  character(len=*), parameter :: code_version = 'micarus 0.3 (mock)'

  !---------------------------------------------------------------- parallel
  integer :: mi_comm = MPI_COMM_NULL   ! solver communicator (a dup of what we were given)
  integer :: myrank = 0, nranks = 1
  logical :: owns_mpi = .false.        ! we called MPI_Init, so we call MPI_Finalize
  logical :: initialized = .false.

  !---------------------------------------------------------------- &control
  character(len=64)  :: case_name = 'micarus'
  character(len=256) :: env_file = 'env.dat'
  character(len=256) :: bprime_file = 'bprime.dat'
  character(len=256) :: output_dir = '.'
  real(rk) :: t_start = 0d0, t_end = 60d0
  real(rk) :: dt_init = 1d-3, dt_min = 1d-7, dt_max = 0.05d0, dt_grow = 1.25d0
  real(rk) :: dtemp_max = 150d0       ! largest |dT| per step before the step is rejected [K]
  integer  :: max_picard = 12, max_cuts = 12
  real(rk) :: picard_tol = 1d-2       ! [K]
  real(rk) :: lin_tol = 1d-9
  integer  :: lin_maxit = 400
  real(rk) :: output_interval = 1d0   ! [s]
  integer  :: log_every = 200         ! steps
  logical  :: restart = .false.
  logical  :: write_restart_at_end = .true.

  !---------------------------------------------------------------- &mesh
  ! A spherical-cap heatshield segment, theta in [0, theta_max], phi in
  ! [0, phi_span], thickness radially. Stored and solved as an unstructured
  ! cell/face list; the generator is just the cheapest way to get one.
  integer  :: n_theta = 24, n_phi = 6, n_depth = 50
  real(rk) :: nose_radius = 0.22d0, thickness = 0.04d0
  real(rk) :: theta_max_deg = 60d0, phi_span_deg = 90d0
  real(rk) :: stretch = 1.06d0        ! geometric growth of radial spacing from the surface

  !---------------------------------------------------------------- &material
  ! Three-component decomposition (two resin reactions + a reinforcing
  ! fiber), rho_s = gamma*(rho_A + rho_B) + (1 - gamma)*rho_C, each
  !   d rho_i / dt = -A_i exp(-E_i/T) rho_v,i ((rho_i - rho_c,i)/rho_v,i)^n_i
  real(rk) :: gamma_resin = 0.5d0
  real(rk) :: rho_vi(3) = [80d0, 200d0, 280d0]
  real(rk) :: rho_ci(3) = [0d0, 120d0, 280d0]
  real(rk) :: a_pre(3)  = [1.2d4, 4.48d9, 0d0]   ! 1/s
  real(rk) :: e_over_r(3) = [8556d0, 20444d0, 0d0] ! K
  real(rk) :: n_order(3) = [3d0, 3d0, 1d0]
  real(rk) :: h_pyro = 1.2d6           ! J/kg of gas produced (endothermic)
  real(rk) :: k_v0 = 0.12d0, k_v1 = 1.0d-4, k_c0 = 0.30d0, k_c1 = 1.5d-4, k_rad = 2.0d-11
  real(rk) :: cp_v = 1300d0, cp_c = 1700d0
  real(rk) :: eps_v = 0.85d0, eps_c = 0.90d0, absorptivity = 0.90d0
  real(rk) :: phi_v = 0.80d0, phi_c = 0.85d0     ! porosity
  real(rk) :: perm_v = 1.0d-11, perm_c = 2.0d-11 ! m^2
  real(rk) :: mw_gas = 0.020d0, mu_gas = 3.0d-5, cp_gas = 2200d0, h_gas0 = -1.0d6

  !---------------------------------------------------------------- &boundary
  character(len=16) :: bc_source = 'table'   ! 'table' (env_file) | 'external' (set by a host code)
  logical  :: blowing_correction = .true.    ! apply ch/ch0 = phi/(e^phi - 1) internally
  real(rk) :: lambda_blow = 0.5d0
  real(rk) :: t_init = 300d0, p_init = 101325d0, t_inf = 300d0

  !---------------------------------------------------------------- &probes
  integer, parameter :: max_tc = 16
  integer  :: n_tc = 0
  real(rk) :: tc_theta_deg(max_tc) = 0d0, tc_phi_deg(max_tc) = 0d0, tc_depth(max_tc) = 0d0

  !---------------------------------------------------------------- mesh (local partition)
  integer :: ncol_g                    ! global number of columns (= surface faces)
  integer :: col_lo, col_hi            ! owned global columns [col_lo, col_hi]
  integer :: n_col_own, n_col_ghost
  integer, allocatable :: ghost_cols(:)        ! global ids of ghost columns
  integer :: n_own, n_ghost, n_tot             ! cells
  integer, allocatable :: c_it(:), c_jp(:), c_k(:)   ! global theta, phi, layer index of each cell
  real(rk), allocatable :: c_vol(:), c_rmid(:), c_r1(:), c_r2(:)
  integer :: nf
  integer, allocatable :: f_o(:), f_n(:)       ! owner (always owned) and neighbour cell
  real(rk), allocatable :: f_area(:), f_dist(:)
  real(rk) :: r_inner
  real(rk), allocatable :: s_node(:)           ! 0..n_depth radial node fractions (0 = surface)
  ! surface faces = owned columns, cell 1 of each
  integer :: ns
  integer, allocatable :: s_cell(:)
  real(rk), allocatable :: s_area(:), s_theta(:), s_phi(:), s_dhalf(:)

  ! halo
  integer :: n_nbr
  integer, allocatable :: nbr_rank(:), snd_ptr(:), rcv_ptr(:), snd_idx(:), rcv_idx(:)

  !---------------------------------------------------------------- fields
  real(rk), allocatable :: temp(:), pres(:), rho_s(:), r_out(:)   ! n_tot (ghosts included)
  real(rk), allocatable :: rho_i(:,:)                               ! (3, n_own)
  real(rk), allocatable :: f_mdot(:)                                ! face gas mass flow o->n [kg/s]
  ! surface (per owned column)
  real(rk), allocatable :: s_ch0(:), s_ch(:), s_hr(:), s_pe(:), s_qrad(:)
  real(rk), allocatable :: s_tw(:), s_mg(:), s_mc(:), s_rec(:), s_qcond(:), s_bc(:)

  !---------------------------------------------------------------- time
  real(rk) :: time = 0d0, dt = 1d-3
  integer  :: istep = 0, n_reject = 0, n_output = 0
  integer  :: lin_iters_total = 0, picard_total = 0

  !---------------------------------------------------------------- tables
  integer :: env_ns = 0, env_nt = 0
  real(rk), allocatable :: env_theta(:), env_time(:), env_val(:,:,:)  ! (4, ns, nt): ch0, h_r, p_e, q_rad
  integer :: bp_np = 0, bp_ng = 0, bp_nt = 0
  real(rk), allocatable :: bp_p(:), bp_g(:), bp_t(:), bp_bc(:,:,:), bp_hw(:,:,:)  ! (nt, ng, np)

end module mi_globals
