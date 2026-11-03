!=======================================================================
!  mi_output: the code's own output. A history file, thermocouple traces,
!  Tecplot surface snapshots (gathered to rank 0), and per-rank binary
!  restart files. The restart record doubles as the in-memory state
!  buffer a host code can save and restore.
!=======================================================================
module mi_output
  use mi_globals
  use mi_parallel, only: mi_fatal, halo, gsum, gmax
  use mi_mesh, only: update_geometry, local_column, cell_index
  implicit none
  private
  public :: open_files, close_files, write_output, log_step, banner, &
            state_size, pack_state, unpack_state, write_restart, read_restart

  integer, save :: u_hist = -1, u_tc = -1

contains

  function path(name) result(p)
    character(len=*), intent(in) :: name
    character(len=512) :: p
    p = trim(output_dir)//'/'//trim(case_name)//name
  end function

  subroutine open_files()
    logical :: append
    if (myrank /= 0) return
    append = restart
    if (append) then
      open(newunit=u_hist, file=path('_hist.dat'), position='append', action='write')
      open(newunit=u_tc, file=path('_tc.dat'), position='append', action='write')
    else
      open(newunit=u_hist, file=path('_hist.dat'), status='replace', action='write')
      write(u_hist, '(a)') '# time step dt Tw_stag mdot_g_stag mdot_c_stag recession_stag Tw_max q_cond_stag'
      open(newunit=u_tc, file=path('_tc.dat'), status='replace', action='write')
      write(u_tc, '(a,i0,a)') '# time, then ', n_tc, ' thermocouple temperatures [K]'
    end if
  end subroutine

  subroutine close_files()
    if (u_hist > 0) close(u_hist)
    if (u_tc > 0) close(u_tc)
    u_hist = -1; u_tc = -1
  end subroutine

  subroutine banner()
    integer :: u, ierr, ncell, nface
    ncell = nint(gsum(real(n_own, rk)))
    nface = nint(gsum(real(nf, rk)))
    if (myrank == 0) then
      write(*, '(a)') ' ================================================================'
      write(*, '(2a)') '  ', code_version
      write(*, '(a)') '  3D unstructured finite-volume pyrolyzing-ablator response'
      write(*, '(a)') ' ================================================================'
      write(*, '(a,i0,a,i0,a,i0,a)') '  ranks ', nranks, ', cells ', ncell, ', faces ', nface, ''
      write(*, '(a,i0,a,i0,a,i0)') '  surface ', ncol_g, ' columns (', n_theta, ' x ', n_phi
      write(*, '(4a)') '  boundary source: ', trim(bc_source), ', blowing correction: ', &
                       merge('internal', 'external', blowing_correction)
      open(newunit=u, file=path('.echo'), status='replace', action='write')
      call echo(u)
      close(u)
    end if
    call MPI_Barrier(mi_comm, ierr)
  contains
    subroutine echo(u)
      use mi_input, only: echo_input
      integer, intent(in) :: u
      call echo_input(u)
    end subroutine
  end subroutine

  subroutine log_step(picard, lin)
    integer, intent(in) :: picard, lin
    real(rk) :: twmax
    twmax = gmax(maxval(s_tw))
    if (myrank == 0) write(*, '(a,i8,a,f10.4,a,es9.2,a,f8.1,a,i3,a,i5,a,i0)') &
      ' step', istep, '  t', time, '  dt', dt, '  Tw_max', twmax, '  picard', picard, &
      '  lin', lin, '  rejected', n_reject
  end subroutine

  !> History line, thermocouples, and a Tecplot surface snapshot.
  subroutine write_output()
    real(rk), allocatable :: mine(:), all(:)
    integer, allocatable :: counts(:), displs(:)
    real(rk) :: tc(max_tc), twmax, dth, dph, r
    integer :: s, i, ierr, nv, g, lc, k, c, u, it, jp
    character(len=8) :: tag
    n_output = n_output + 1
    twmax = gmax(maxval(s_tw))

    ! thermocouples: owner of the column contributes, everyone else adds 0
    dth = theta_max_deg / n_theta; dph = phi_span_deg / n_phi
    tc = 0
    do i = 1, n_tc
      it = min(n_theta, int(tc_theta_deg(i) / dth) + 1)
      jp = min(n_phi, int(tc_phi_deg(i) / dph) + 1)
      g = (it - 1) * n_phi + jp
      if (g < col_lo .or. g > col_hi) cycle
      lc = local_column(g)
      r = nose_radius - tc_depth(i)
      c = cell_index(lc, 1)
      if (r >= c_r1(c)) then
        tc(i) = s_tw(lc)                 ! the surface has receded past the probe
      else
        do k = 1, n_depth
          c = cell_index(lc, k)
          if (r >= c_r2(c)) exit
        end do
        tc(i) = temp(c)
      end if
    end do
    if (n_tc > 0) call MPI_Reduce(copy_of(tc), tc, n_tc, MPI_DOUBLE_PRECISION, &
                                  MPI_SUM, 0, mi_comm, ierr)

    ! surface snapshot, gathered in global column order
    nv = 10
    allocate(mine(nv * ns))
    do s = 1, ns
      mine(nv*(s-1)+1:nv*s) = [real(col_lo + s - 1, rk), s_theta(s) * 180 / pi, s_phi(s) * 180 / pi, &
        s_tw(s), s_mg(s), s_mc(s), s_rec(s), s_qcond(s), s_ch(s), s_bc(s)]
    end do
    allocate(counts(nranks), displs(nranks))
    call MPI_Gather(nv * ns, 1, MPI_INTEGER, counts, 1, MPI_INTEGER, 0, mi_comm, ierr)
    if (myrank == 0) then
      displs(1) = 0
      do i = 2, nranks
        displs(i) = displs(i - 1) + counts(i - 1)
      end do
      allocate(all(sum(counts)))
    else
      allocate(all(1))
    end if
    call MPI_Gatherv(mine, nv * ns, MPI_DOUBLE_PRECISION, all, counts, displs, &
                     MPI_DOUBLE_PRECISION, 0, mi_comm, ierr)

    if (myrank /= 0) return
    write(u_hist, '(es22.14,i9,8es22.14)') time, istep, dt, s_tw(1), s_mg(1), s_mc(1), &
      s_rec(1), twmax, s_qcond(1)
    flush(u_hist)
    if (n_tc > 0) then
      write(u_tc, '(17es22.14)') time, tc(1:n_tc)
      flush(u_tc)
    end if
    write(tag, '(i4.4)') n_output
    open(newunit=u, file=path('_surf_'//trim(tag)//'.dat'), status='replace', action='write')
    write(u, '(a)') 'TITLE = "micarus surface"'
    write(u, '(a)') 'VARIABLES = "col" "theta_deg" "phi_deg" "Tw" "mdot_g" "mdot_c" "recession" "q_cond" "ch" "Bc"'
    write(u, '(a,es22.14,a,i0,a,i0,a)') 'ZONE T="t=', time, '", I=', n_phi, ', J=', n_theta, ', F=POINT'
    do s = 1, ncol_g
      write(u, '(10es22.14)') all(nv*(s-1)+1:nv*s)
    end do
    close(u)
  contains
    ! a separate send buffer (send and receive buffers may not alias)
    function copy_of(x) result(y)
      real(rk), intent(in) :: x(:)
      real(rk) :: y(size(x))
      y = x
    end function
  end subroutine

  ! ------------------------------------------------------------ state record

  integer function state_size()
    state_size = 6 + 6 * n_own + 11 * ns
  end function

  subroutine pack_state(buf)
    real(rk), intent(out) :: buf(:)
    integer :: p
    buf(1:6) = [time, dt, real(istep, rk), real(n_reject, rk), real(n_output, rk), real(nranks, rk)]
    p = 7
    call put(temp(1:n_own)); call put(pres(1:n_own)); call put(r_out(1:n_own))
    call put(rho_i(1, :)); call put(rho_i(2, :)); call put(rho_i(3, :))
    call put(s_tw); call put(s_mg); call put(s_mc); call put(s_rec); call put(s_qcond)
    call put(s_ch); call put(s_bc); call put(s_ch0); call put(s_hr); call put(s_pe); call put(s_qrad)
  contains
    subroutine put(x)
      real(rk), intent(in) :: x(:)
      buf(p:p + size(x) - 1) = x
      p = p + size(x)
    end subroutine
  end subroutine

  subroutine unpack_state(buf)
    real(rk), intent(in) :: buf(:)
    integer :: p, c
    if (nint(buf(6)) /= nranks) call mi_fatal('state record was written with a different rank count')
    time = buf(1); dt = buf(2); istep = nint(buf(3)); n_reject = nint(buf(4)); n_output = nint(buf(5))
    p = 7
    call get(temp(1:n_own)); call get(pres(1:n_own)); call get(r_out(1:n_own))
    call get(rho_i(1, :)); call get(rho_i(2, :)); call get(rho_i(3, :))
    call get(s_tw); call get(s_mg); call get(s_mc); call get(s_rec); call get(s_qcond)
    call get(s_ch); call get(s_bc); call get(s_ch0); call get(s_hr); call get(s_pe); call get(s_qrad)
    do c = 1, n_own
      rho_s(c) = gamma_resin * (rho_i(1, c) + rho_i(2, c)) + (1 - gamma_resin) * rho_i(3, c)
    end do
    call halo(temp); call halo(pres); call halo(rho_s)
    call update_geometry()
  contains
    subroutine get(x)
      real(rk), intent(out) :: x(:)
      x = buf(p:p + size(x) - 1)
      p = p + size(x)
    end subroutine
  end subroutine

  !> One binary restart file per rank, the way most legacy MPI codes do it:
  !> a restart must use the same rank count.
  subroutine write_restart()
    real(rk), allocatable :: buf(:)
    integer :: u
    character(len=8) :: tag
    allocate(buf(state_size()))
    call pack_state(buf)
    write(tag, '(i4.4)') myrank
    open(newunit=u, file=path('.rst.'//trim(tag)), access='stream', form='unformatted', &
         status='replace', action='write')
    write(u) 20251103, nranks, n_own, ns, size(buf)
    write(u) buf
    close(u)
    if (myrank == 0) write(*, '(a,f10.4,a,i0,a)') ' restart written at t =', time, ' (', nranks, ' files)'
  end subroutine

  subroutine read_restart()
    real(rk), allocatable :: buf(:)
    integer :: u, magic, nr, no, nsr, n, ios
    character(len=8) :: tag
    write(tag, '(i4.4)') myrank
    open(newunit=u, file=path('.rst.'//trim(tag)), access='stream', form='unformatted', &
         status='old', action='read', iostat=ios)
    if (ios /= 0) call mi_fatal('cannot open restart file '//trim(path('.rst.'//trim(tag))))
    read(u) magic, nr, no, nsr, n
    if (magic /= 20251103) call mi_fatal('not a micarus restart file')
    if (nr /= nranks) call mi_fatal('restart was written with a different number of ranks')
    if (no /= n_own .or. nsr /= ns .or. n /= state_size()) call mi_fatal('restart does not match the mesh')
    allocate(buf(n))
    read(u) buf
    close(u)
    call unpack_state(buf)
    if (myrank == 0) write(*, '(a,f10.4)') ' restarted from t =', time
  end subroutine

end module mi_output
