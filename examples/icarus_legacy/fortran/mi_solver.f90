!=======================================================================
!  mi_solver: line-preconditioned BiCGSTAB on the face-based operator
!
!    (A x)_i = diag_i x_i + sum_{faces f with owner i} a_on(f) x_{n(f)}
!                         + sum_{faces f with neighbour i} a_no(f) x_{o(f)}
!
!  The preconditioner solves each through-thickness column exactly (a
!  tridiagonal "line" solve) and drops the lateral couplings: the heat
!  goes mostly inward, and the radial spacing is stretched, so this is
!  what makes the solver converge in a handful of iterations. Columns are
!  never split across ranks, so it needs no communication. One halo
!  exchange per matrix-vector product and MPI_Allreduce for every inner
!  product, as in any distributed Krylov solver.
!=======================================================================
module mi_solver
  use mi_globals
  use mi_parallel, only: halo, gsum
  implicit none
  private
  public :: bicgstab

contains

  subroutine matvec(diag, a_on, a_no, x, y)
    real(rk), intent(in) :: diag(:), a_on(:), a_no(:)
    real(rk), intent(inout) :: x(:)     ! n_tot; ghosts are refreshed here
    real(rk), intent(out) :: y(:)       ! n_own
    integer :: f, o, n
    call halo(x)
    y(1:n_own) = diag(1:n_own) * x(1:n_own)
    do f = 1, nf
      o = f_o(f); n = f_n(f)
      y(o) = y(o) + a_on(f) * x(n)
      if (n <= n_own) y(n) = y(n) + a_no(f) * x(o)
    end do
  end subroutine

  !> Through-thickness (radial) couplings of each owned cell.
  subroutine line_setup(a_on, a_no, lo, up)
    real(rk), intent(in) :: a_on(:), a_no(:)
    real(rk), intent(out) :: lo(:), up(:)
    integer :: f, o, n
    lo = 0; up = 0
    do f = 1, nf
      o = f_o(f); n = f_n(f)
      if (n == o + 1 .and. c_k(n) == c_k(o) + 1) then
        up(o) = a_on(f)
        lo(n) = a_no(f)
      end if
    end do
  end subroutine

  !> z = M^-1 r, one Thomas solve per column.
  subroutine line_apply(diag, lo, up, r, z)
    real(rk), intent(in) :: diag(:), lo(:), up(:), r(:)
    real(rk), intent(inout) :: z(:)
    real(rk) :: cp(n_depth), dp(n_depth), m
    integer :: lc, k, c0
    do lc = 1, n_col_own
      c0 = (lc - 1) * n_depth
      cp(1) = up(c0 + 1) / diag(c0 + 1)
      dp(1) = r(c0 + 1) / diag(c0 + 1)
      do k = 2, n_depth
        m = diag(c0 + k) - lo(c0 + k) * cp(k - 1)
        cp(k) = up(c0 + k) / m
        dp(k) = (r(c0 + k) - lo(c0 + k) * dp(k - 1)) / m
      end do
      z(c0 + n_depth) = dp(n_depth)
      do k = n_depth - 1, 1, -1
        z(c0 + k) = dp(k) - cp(k) * z(c0 + k + 1)
      end do
    end do
  end subroutine

  real(rk) function dot(a, b)
    real(rk), intent(in) :: a(:), b(:)
    dot = gsum(sum(a(1:n_own) * b(1:n_own)))
  end function

  !> Solve A x = b. x holds the initial guess on entry (n_tot long).
  !> Returns the iteration count, or -1 if it did not converge.
  integer function bicgstab(diag, a_on, a_no, b, x) result(its)
    real(rk), intent(in) :: diag(:), a_on(:), a_no(:), b(:)
    real(rk), intent(inout) :: x(:)
    real(rk), allocatable :: r(:), r0(:), p(:), v(:), s(:), t(:), ph(:), sh(:), lo(:), up(:)
    real(rk) :: rho, rho_old, alpha, omega, beta, bnorm, rnorm, tt
    integer :: k
    allocate(r(n_own), r0(n_own), v(n_own), t(n_own), p(n_own))
    allocate(ph(n_tot), sh(n_tot), s(n_own), lo(n_own), up(n_own))
    call line_setup(a_on, a_no, lo, up)
    call matvec(diag, a_on, a_no, x, r)
    r = b(1:n_own) - r
    r0 = r
    bnorm = sqrt(dot(b, b))
    if (bnorm == 0) bnorm = 1
    rnorm = sqrt(dot(r, r))
    its = 0
    if (rnorm / bnorm < lin_tol) return
    rho_old = 1; alpha = 1; omega = 1
    v = 0; p = 0; ph = 0; sh = 0
    do k = 1, lin_maxit
      rho = dot(r0, r)
      if (rho == 0) exit
      beta = (rho / rho_old) * (alpha / omega)
      p = r + beta * (p - omega * v)
      call line_apply(diag, lo, up, p, ph)
      call matvec(diag, a_on, a_no, ph, v)
      alpha = rho / dot(r0, v)
      s = r - alpha * v
      if (sqrt(dot(s, s)) / bnorm < lin_tol) then
        x(1:n_own) = x(1:n_own) + alpha * ph(1:n_own)
        its = k
        call halo(x)
        return
      end if
      call line_apply(diag, lo, up, s, sh)
      call matvec(diag, a_on, a_no, sh, t)
      tt = dot(t, t)
      omega = dot(t, s) / tt
      x(1:n_own) = x(1:n_own) + alpha * ph(1:n_own) + omega * sh(1:n_own)
      r = s - omega * t
      rnorm = sqrt(dot(r, r))
      if (rnorm / bnorm < lin_tol) then
        its = k
        call halo(x)
        return
      end if
      rho_old = rho
    end do
    its = -1
    call halo(x)
  end function

end module mi_solver
