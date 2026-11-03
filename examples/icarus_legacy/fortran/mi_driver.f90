!=======================================================================
!  mi_driver: initialise, advance, finalise.
!
!  The program's main loop (mi_run) is written on top of mi_advance_to,
!  which owns the adaptive time step: it subcycles with its own dt, cuts
!  the step on rejection, and lands exactly on the requested time.
!=======================================================================
module mi_driver
  use mi_globals
  use mi_parallel, only: mi_fatal
  use mi_input, only: read_input, read_env_table, read_bprime_table
  use mi_mesh, only: build_mesh, update_geometry
  use mi_physics, only: try_step, set_table_environment, init_fields
  use mi_output
  implicit none
  private
  public :: mi_initialize, mi_advance_to, mi_run, mi_finalize

  integer, parameter, public :: ADV_OK = 0, ADV_DT_UNDERFLOW = 2, ADV_BURN_THROUGH = 3

contains

  !> comm: the communicator to run on (MPI_COMM_WORLD from the stand-alone
  !> program; a host passes its own). override: optional second namelist
  !> file read after the first ('' for none).
  subroutine mi_initialize(comm, input_file, override_file)
    integer, intent(in) :: comm
    character(len=*), intent(in) :: input_file, override_file
    logical :: flag
    integer :: ierr, provided
    if (initialized) call mi_fatal('micarus is already initialised in this process')
    ! mesh and field arrays live for the life of the process
    if (allocated(temp)) call mi_fatal('micarus can only be initialised once per process')
    call MPI_Initialized(flag, ierr)
    if (.not. flag) then
      call MPI_Init_thread(MPI_THREAD_FUNNELED, provided, ierr)
      owns_mpi = .true.
    end if
    call MPI_Comm_dup(comm, mi_comm, ierr)
    call MPI_Comm_rank(mi_comm, myrank, ierr)
    call MPI_Comm_size(mi_comm, nranks, ierr)

    call read_input(input_file)
    if (len_trim(override_file) > 0) call read_input(override_file)
    call read_bprime_table()
    select case (trim(bc_source))
    case ('table');    call read_env_table()
    case ('external')
    case default;      call mi_fatal('&boundary bc_source must be ''table'' or ''external''')
    end select

    call build_mesh()
    call init_fields()
    call update_geometry()
    time = t_start
    dt = dt_init
    istep = 0; n_reject = 0; n_output = 0; lin_iters_total = 0; picard_total = 0
    if (restart) call read_restart()
    call banner()
    call open_files()
    initialized = .true.
  end subroutine

  !> Advance to exactly t_target with the code's own adaptive step.
  subroutine mi_advance_to(t_target, status)
    real(rk), intent(in) :: t_target
    integer, intent(out) :: status
    real(rk) :: h
    integer :: st, cuts, pits, lits
    logical :: last
    status = ADV_OK
    cuts = 0
    do while (time < t_target)
      last = (t_target - time) <= 1.001d0 * dt
      if (last) then
        h = t_target - time
      else
        h = dt
      end if
      if (trim(bc_source) == 'table') call set_table_environment(time + h)
      st = try_step(h, pits, lits)
      if (st == 1) then
        n_reject = n_reject + 1
        cuts = cuts + 1
        dt = 0.5d0 * h
        if (dt < dt_min .or. cuts > max_cuts) then
          status = ADV_DT_UNDERFLOW
          return
        end if
        cycle
      end if
      cuts = 0
      if (last) then
        time = t_target
      else
        time = time + h
        dt = min(dt * dt_grow, dt_max)
      end if
      istep = istep + 1
      picard_total = picard_total + pits
      lin_iters_total = lin_iters_total + lits
      if (log_every > 0) then
        if (mod(istep, log_every) == 0) call log_step(pits, lits)
      end if
      if (st == 3) then
        status = ADV_BURN_THROUGH
        return
      end if
    end do
  end subroutine

  !> The stand-alone program: run the input deck to t_end.
  subroutine mi_run()
    real(rk) :: target
    integer :: status
    character(len=32) :: tstr
    do while (time < t_end)
      target = min(t_start + (n_output + 1) * output_interval, t_end)
      call mi_advance_to(target, status)
      if (status /= ADV_OK) then
        write(tstr, '(f12.5)') time
        if (status == ADV_BURN_THROUGH) call mi_fatal('burn-through at t = '//trim(adjustl(tstr)))
        call mi_fatal('time step underflow at t = '//trim(adjustl(tstr)))
      end if
      call write_output()
    end do
    if (write_restart_at_end) call write_restart()
    if (myrank == 0) write(*, '(a,f10.4,a,i0,a,i0,a,i0,a)') ' done: t =', time, ', ', istep, &
      ' steps, ', n_reject, ' rejected, ', lin_iters_total, ' linear iterations'
  end subroutine

  subroutine mi_finalize()
    integer :: ierr
    if (.not. initialized) return
    call close_files()
    call MPI_Comm_free(mi_comm, ierr)
    mi_comm = MPI_COMM_NULL
    if (allocated(env_time)) deallocate(env_theta, env_time, env_val)
    if (allocated(bp_p)) deallocate(bp_p, bp_g, bp_t, bp_bc, bp_hw)
    initialized = .false.
    if (owns_mpi) call MPI_Finalize(ierr)
  end subroutine

end module mi_driver
