!=======================================================================
!  mi_parallel: fatal errors, halo exchange, global reductions.
!=======================================================================
module mi_parallel
  use iso_c_binding, only: c_funptr, c_null_funptr, c_associated, c_f_procpointer, &
                           c_char, c_null_char, c_int
  use mi_globals
  implicit none
  private
  public :: mi_fatal, halo, gsum, gmax, gmin, col_owner, error_hook

  !> Optional "last words" callback a host program can install. Legacy codes
  !> stop the world on a fatal error; this lets the host log or flush first.
  type(c_funptr), save :: error_hook = c_null_funptr

  abstract interface
    subroutine hook_iface(rank, msg) bind(C)
      import :: c_int, c_char
      integer(c_int), value :: rank
      character(kind=c_char), intent(in) :: msg(*)
    end subroutine
  end interface

contains

  !> Print, give the host a chance to react, and stop every rank.
  subroutine mi_fatal(msg)
    character(len=*), intent(in) :: msg
    procedure(hook_iface), pointer :: hook
    integer :: ierr
    write(0, '(a,i0,2a)') ' *** micarus FATAL (rank ', myrank, '): ', trim(msg)
    flush(0)
    if (c_associated(error_hook)) then
      call c_f_procpointer(error_hook, hook)
      call hook(int(myrank, c_int), trim(msg)//c_null_char)
    end if
    if (mi_comm /= MPI_COMM_NULL) then
      call MPI_Abort(mi_comm, 1, ierr)
    else
      call MPI_Abort(MPI_COMM_WORLD, 1, ierr)
    end if
    stop 1
  end subroutine

  !> Owner rank of a global column (contiguous block partition).
  pure integer function col_owner(g)
    integer, intent(in) :: g
    integer :: r
    col_owner = nranks - 1
    do r = 0, nranks - 1
      if (g <= ((r + 1) * ncol_g) / nranks) then
        col_owner = r
        return
      end if
    end do
  end function

  !> Fill the ghost entries of a cell array from their owners.
  subroutine halo(x)
    real(rk), intent(inout) :: x(:)
    real(rk), allocatable :: sbuf(:), rbuf(:)
    integer :: q, i, ierr, nreq
    integer, allocatable :: req(:)
    if (n_nbr == 0) return
    allocate(sbuf(snd_ptr(n_nbr + 1) - 1), rbuf(rcv_ptr(n_nbr + 1) - 1), req(2 * n_nbr))
    nreq = 0
    do q = 1, n_nbr
      nreq = nreq + 1
      call MPI_Irecv(rbuf(rcv_ptr(q)), rcv_ptr(q + 1) - rcv_ptr(q), MPI_DOUBLE_PRECISION, &
                     nbr_rank(q), 77, mi_comm, req(nreq), ierr)
    end do
    do q = 1, n_nbr
      do i = snd_ptr(q), snd_ptr(q + 1) - 1
        sbuf(i) = x(snd_idx(i))
      end do
      nreq = nreq + 1
      call MPI_Isend(sbuf(snd_ptr(q)), snd_ptr(q + 1) - snd_ptr(q), MPI_DOUBLE_PRECISION, &
                     nbr_rank(q), 77, mi_comm, req(nreq), ierr)
    end do
    call MPI_Waitall(nreq, req, MPI_STATUSES_IGNORE, ierr)
    do i = 1, rcv_ptr(n_nbr + 1) - 1
      x(rcv_idx(i)) = rbuf(i)
    end do
  end subroutine

  real(rk) function gsum(x)
    real(rk), intent(in) :: x
    integer :: ierr
    call MPI_Allreduce(x, gsum, 1, MPI_DOUBLE_PRECISION, MPI_SUM, mi_comm, ierr)
  end function

  real(rk) function gmax(x)
    real(rk), intent(in) :: x
    integer :: ierr
    call MPI_Allreduce(x, gmax, 1, MPI_DOUBLE_PRECISION, MPI_MAX, mi_comm, ierr)
  end function

  real(rk) function gmin(x)
    real(rk), intent(in) :: x
    integer :: ierr
    call MPI_Allreduce(x, gmin, 1, MPI_DOUBLE_PRECISION, MPI_MIN, mi_comm, ierr)
  end function

end module mi_parallel
