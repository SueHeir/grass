!=======================================================================
!  micarus_capi: the C interface a host program (here, GRASS) calls.
!
!  This file is the whole of the Fortran-side adapter. It sits beside the
!  legacy code, not inside it: everything here calls routines the
!  stand-alone program already uses (mi_initialize, mi_advance_to, the
!  restart record) or reads/writes the surface arrays in mi_globals.
!  The changes the legacy code itself needed are listed in the README.
!
!  Conventions: arrays are over this rank's surface faces in the order
!  micarus_surface_geometry reports; strings are NUL-terminated.
!=======================================================================
module micarus_capi
  use iso_c_binding
  use mi_globals
  use mi_driver, only: mi_initialize, mi_advance_to, mi_finalize
  use mi_output, only: state_size, pack_state, unpack_state, write_output, write_restart
  use mi_parallel, only: error_hook
  implicit none
  private

contains

  function c2f(cs) result(fs)
    character(kind=c_char), intent(in) :: cs(*)
    character(len=1024) :: fs
    integer :: i
    fs = ' '
    do i = 1, len(fs)
      if (cs(i) == c_null_char) exit
      fs(i:i) = cs(i)
    end do
  end function

  !> comm_f: a Fortran MPI communicator handle (MPI_Comm_c2f of the host's
  !> communicator). override: '' or a second namelist file.
  integer(c_int) function micarus_init(comm_f, input, override) bind(C)
    integer(c_int), value :: comm_f
    character(kind=c_char), intent(in) :: input(*), override(*)
    call mi_initialize(int(comm_f), trim(c2f(input)), trim(c2f(override)))
    micarus_init = 0
  end function

  !> Install a callback run (on the failing rank) just before a fatal error
  !> aborts the job: void hook(int rank, const char *msg).
  subroutine micarus_set_error_hook(f) bind(C)
    type(c_funptr), value :: f
    error_hook = f
  end subroutine

  !> 0 ok, 2 time-step underflow, 3 burn-through.
  integer(c_int) function micarus_advance_to(t) bind(C)
    real(c_double), value :: t
    integer :: status
    call mi_advance_to(t, status)
    micarus_advance_to = status
  end function

  real(c_double) function micarus_time() bind(C)
    micarus_time = time
  end function

  !> out = [t_start, t_end, output_interval, dt]
  subroutine micarus_control(out) bind(C)
    real(c_double), intent(out) :: out(4)
    out = [t_start, t_end, output_interval, dt]
  end subroutine

  !> out = [steps, rejected steps, picard iterations, linear iterations, ranks, global surface faces]
  subroutine micarus_counters(out) bind(C)
    integer(c_int), intent(out) :: out(6)
    out = [istep, n_reject, picard_total, lin_iters_total, nranks, ncol_g]
  end subroutine

  integer(c_int) function micarus_n_surface() bind(C)
    micarus_n_surface = ns
  end function

  !> Global face id (1-based), body angle theta and azimuth phi [rad], area [m2].
  subroutine micarus_surface_geometry(gid, theta, phi, area) bind(C)
    integer(c_int), intent(out) :: gid(*)
    real(c_double), intent(out) :: theta(*), phi(*), area(*)
    integer :: s
    do s = 1, ns
      gid(s) = col_lo + s - 1
      theta(s) = s_theta(s); phi(s) = s_phi(s); area(s) = s_area(s)
    end do
  end subroutine

  !> Surface environment for the next advance (bc_source = 'external').
  !> ch [kg/m2/s] is taken as-is unless &boundary blowing_correction is on.
  integer(c_int) function micarus_set_environment(ch, h_r, p_e, q_rad) bind(C)
    real(c_double), intent(in) :: ch(*), h_r(*), p_e(*), q_rad(*)
    micarus_set_environment = 1
    if (trim(bc_source) /= 'external') return
    s_ch0 = ch(1:ns); s_hr = h_r(1:ns); s_pe = p_e(1:ns); s_qrad = q_rad(1:ns)
    micarus_set_environment = 0
  end function

  !> Per face, 8 values: Tw, mdot_gas, mdot_char, recession, q_cond, ch, B'c, thickness.
  subroutine micarus_get_surface(out) bind(C)
    real(c_double), intent(out) :: out(*)
    integer :: s
    do s = 1, ns
      out(8*(s-1)+1:8*s) = [s_tw(s), s_mg(s), s_mc(s), s_rec(s), s_qcond(s), s_ch(s), s_bc(s), &
                            r_out(s_cell(s)) - r_inner]
    end do
  end subroutine

  integer(c_int) function micarus_state_size() bind(C)
    micarus_state_size = state_size()
  end function

  subroutine micarus_save_state(buf) bind(C)
    real(c_double), intent(out) :: buf(*)
    call pack_state(buf(1:state_size()))
  end subroutine

  subroutine micarus_restore_state(buf) bind(C)
    real(c_double), intent(in) :: buf(*)
    call unpack_state(buf(1:state_size()))
  end subroutine

  subroutine micarus_write_output() bind(C)
    call write_output()
  end subroutine

  subroutine micarus_write_restart() bind(C)
    call write_restart()
  end subroutine

  subroutine micarus_finalize() bind(C)
    call mi_finalize()
  end subroutine

end module micarus_capi
