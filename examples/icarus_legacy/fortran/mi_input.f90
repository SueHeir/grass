!=======================================================================
!  mi_input: namelist input, the aerothermal environment table, and the
!  B' surface-thermochemistry table.
!=======================================================================
module mi_input
  use mi_globals
  use mi_parallel, only: mi_fatal
  implicit none
  private
  public :: read_input, read_env_table, read_bprime_table, env_lookup, bprime_lookup, echo_input

  namelist /control/ case_name, env_file, bprime_file, output_dir, t_start, t_end, &
       dt_init, dt_min, dt_max, dt_grow, dtemp_max, max_picard, max_cuts, picard_tol, &
       lin_tol, lin_maxit, output_interval, log_every, restart, write_restart_at_end
  namelist /mesh/ n_theta, n_phi, n_depth, nose_radius, thickness, theta_max_deg, &
       phi_span_deg, stretch
  namelist /material/ gamma_resin, rho_vi, rho_ci, a_pre, e_over_r, n_order, h_pyro, &
       k_v0, k_v1, k_c0, k_c1, k_rad, cp_v, cp_c, eps_v, eps_c, absorptivity, &
       phi_v, phi_c, perm_v, perm_c, mw_gas, mu_gas, cp_gas, h_gas0
  namelist /boundary/ bc_source, blowing_correction, lambda_blow, t_init, p_init, t_inf
  namelist /probes/ n_tc, tc_theta_deg, tc_phi_deg, tc_depth

contains

  !> Every rank reads the input deck itself. Groups that are absent keep
  !> their defaults, so a second (override) file can set just a few values.
  subroutine read_input(fname)
    character(len=*), intent(in) :: fname
    integer :: u, ios
    logical :: there
    inquire(file=trim(fname), exist=there)
    if (.not. there) call mi_fatal('input file not found: '//trim(fname))
    open(newunit=u, file=trim(fname), status='old', action='read')
    rewind(u); read(u, nml=control, iostat=ios);  call check(ios, 'control')
    rewind(u); read(u, nml=mesh, iostat=ios);     call check(ios, 'mesh')
    rewind(u); read(u, nml=material, iostat=ios); call check(ios, 'material')
    rewind(u); read(u, nml=boundary, iostat=ios); call check(ios, 'boundary')
    rewind(u); read(u, nml=probes, iostat=ios);   call check(ios, 'probes')
    close(u)
  contains
    subroutine check(ios, grp)
      integer, intent(in) :: ios
      character(len=*), intent(in) :: grp
      if (ios > 0) call mi_fatal('error reading namelist &'//grp//' in '//trim(fname))
    end subroutine
  end subroutine

  subroutine echo_input(u)
    integer, intent(in) :: u
    write(u, nml=control)
    write(u, nml=mesh)
    write(u, nml=material)
    write(u, nml=boundary)
    write(u, nml=probes)
  end subroutine

  !> Aerothermal environment vs. time at a few body stations, the classic
  !> hand-off from a CFD/engineering code run along the trajectory.
  subroutine read_env_table()
    integer :: u, it, s, ios
    character(len=512) :: line
    logical :: there
    inquire(file=trim(env_file), exist=there)
    if (.not. there) call mi_fatal('environment table not found: '//trim(env_file))
    open(newunit=u, file=trim(env_file), status='old', action='read')
    call next_data(u, line)
    read(line, *, iostat=ios) env_ns, env_nt
    if (ios /= 0 .or. env_ns < 1 .or. env_nt < 1) call mi_fatal('bad header in '//trim(env_file))
    allocate(env_theta(env_ns), env_time(env_nt), env_val(4, env_ns, env_nt))
    call next_data(u, line)
    read(line, *, iostat=ios) env_theta
    if (ios /= 0) call mi_fatal('bad station list in '//trim(env_file))
    env_theta = env_theta * pi / 180d0
    do it = 1, env_nt
      call next_data(u, line)
      read(line, *, iostat=ios) env_time(it), (env_val(:, s, it), s = 1, env_ns)
      if (ios /= 0) call mi_fatal('bad record in '//trim(env_file))
    end do
    close(u)
  end subroutine

  !> B'c and wall enthalpy h_w tabulated in (T, B'g, p).
  subroutine read_bprime_table()
    integer :: u, ip, ig, it, ios
    character(len=512) :: line
    logical :: there
    inquire(file=trim(bprime_file), exist=there)
    if (.not. there) call mi_fatal('B'' table not found: '//trim(bprime_file))
    open(newunit=u, file=trim(bprime_file), status='old', action='read')
    call next_data(u, line)
    read(line, *, iostat=ios) bp_np, bp_ng, bp_nt
    if (ios /= 0) call mi_fatal('bad header in '//trim(bprime_file))
    allocate(bp_p(bp_np), bp_g(bp_ng), bp_t(bp_nt))
    allocate(bp_bc(bp_nt, bp_ng, bp_np), bp_hw(bp_nt, bp_ng, bp_np))
    call next_data(u, line); read(line, *) bp_p
    call next_data(u, line); read(line, *) bp_g
    call next_data(u, line); read(line, *) bp_t
    do ip = 1, bp_np
      do ig = 1, bp_ng
        do it = 1, bp_nt
          call next_data(u, line)
          read(line, *, iostat=ios) bp_bc(it, ig, ip), bp_hw(it, ig, ip)
          if (ios /= 0) call mi_fatal('bad record in '//trim(bprime_file))
        end do
      end do
    end do
    close(u)
  end subroutine

  subroutine next_data(u, line)
    integer, intent(in) :: u
    character(len=*), intent(out) :: line
    integer :: ios
    do
      read(u, '(a)', iostat=ios) line
      if (ios /= 0) call mi_fatal('unexpected end of table file')
      line = adjustl(line)
      if (len_trim(line) > 0 .and. line(1:1) /= '#') return
    end do
  end subroutine

  !> ch0, h_r, p_e, q_rad at body angle theta [rad] and time t, linear in both.
  subroutine env_lookup(theta, t, out)
    real(rk), intent(in) :: theta, t
    real(rk), intent(out) :: out(4)
    integer :: i0, i1, j0, j1
    real(rk) :: wt, ws
    call bracket(env_time, env_nt, t, j0, j1, wt)
    call bracket(env_theta, env_ns, theta, i0, i1, ws)
    out = (1 - wt) * ((1 - ws) * env_val(:, i0, j0) + ws * env_val(:, i1, j0)) &
        +      wt  * ((1 - ws) * env_val(:, i0, j1) + ws * env_val(:, i1, j1))
  end subroutine

  !> B'c and h_w at (T, B'g, p): linear in T and B'g, linear in ln p; clamped.
  subroutine bprime_lookup(t, bg, p, bc, hw)
    real(rk), intent(in) :: t, bg, p
    real(rk), intent(out) :: bc, hw
    integer :: i0, i1, j0, j1, k0, k1
    real(rk) :: wt, wg, wp
    call bracket(bp_t, bp_nt, t, i0, i1, wt)
    call bracket(bp_g, bp_ng, bg, j0, j1, wg)
    call bracket(log(bp_p), bp_np, log(max(p, 1d0)), k0, k1, wp)
    bc = blend(bp_bc)
    hw = blend(bp_hw)
  contains
    real(rk) function blend(a)
      real(rk), intent(in) :: a(:,:,:)
      real(rk) :: lo, hi
      lo = (1 - wg) * ((1 - wt) * a(i0, j0, k0) + wt * a(i1, j0, k0)) &
         +      wg  * ((1 - wt) * a(i0, j1, k0) + wt * a(i1, j1, k0))
      hi = (1 - wg) * ((1 - wt) * a(i0, j0, k1) + wt * a(i1, j0, k1)) &
         +      wg  * ((1 - wt) * a(i0, j1, k1) + wt * a(i1, j1, k1))
      blend = (1 - wp) * lo + wp * hi
    end function
  end subroutine

  !> Clamped bracketing search in an ascending table.
  pure subroutine bracket(x, n, v, i0, i1, w)
    integer, intent(in) :: n
    real(rk), intent(in) :: x(n), v
    integer, intent(out) :: i0, i1
    real(rk), intent(out) :: w
    integer :: i
    if (n == 1 .or. v <= x(1)) then
      i0 = 1; i1 = 1; w = 0; return
    end if
    if (v >= x(n)) then
      i0 = n; i1 = n; w = 0; return
    end if
    do i = 1, n - 1
      if (v <= x(i + 1)) exit
    end do
    i0 = i; i1 = i + 1
    w = (v - x(i)) / (x(i + 1) - x(i))
  end subroutine

end module mi_input
