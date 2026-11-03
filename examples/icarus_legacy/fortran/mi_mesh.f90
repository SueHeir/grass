!=======================================================================
!  mi_mesh: build this rank's piece of the unstructured mesh.
!
!  The domain is a spherical-cap segment of the heatshield, decomposed
!  into "columns" (one per surface face) of n_depth cells. Ranks own
!  contiguous blocks of columns plus one layer of ghost columns. After
!  construction everything downstream sees only a cell list and a face
!  list (owner, neighbour, area, distance): the solver does not know the
!  mesh came from a generator.
!=======================================================================
module mi_mesh
  use mi_globals
  use mi_parallel, only: mi_fatal, col_owner, halo
  implicit none
  private
  public :: build_mesh, update_geometry, cell_index, local_column

  integer, allocatable :: f_type(:)    ! 1 radial, 2 theta, 3 phi

contains

  pure integer function gcol(it, jp)
    integer, intent(in) :: it, jp
    gcol = (it - 1) * n_phi + jp
  end function

  !> Local column index of a global column (0 if not present on this rank).
  integer function local_column(g)
    integer, intent(in) :: g
    integer :: i
    local_column = 0
    if (g >= col_lo .and. g <= col_hi) then
      local_column = g - col_lo + 1
      return
    end if
    do i = 1, n_col_ghost
      if (ghost_cols(i) == g) then
        local_column = n_col_own + i
        return
      end if
    end do
  end function

  pure integer function cell_index(lc, k)
    integer, intent(in) :: lc, k
    cell_index = (lc - 1) * n_depth + k
  end function

  subroutine neighbours(g, nb, nn)
    integer, intent(in) :: g
    integer, intent(out) :: nb(4), nn
    integer :: it, jp
    it = (g - 1) / n_phi + 1
    jp = g - (it - 1) * n_phi
    nn = 0
    if (it > 1)       then; nn = nn + 1; nb(nn) = gcol(it - 1, jp); end if
    if (it < n_theta) then; nn = nn + 1; nb(nn) = gcol(it + 1, jp); end if
    if (jp > 1)       then; nn = nn + 1; nb(nn) = gcol(it, jp - 1); end if
    if (jp < n_phi)   then; nn = nn + 1; nb(nn) = gcol(it, jp + 1); end if
  end subroutine

  !> Sorted ghost columns of rank r (computable on any rank: the partition
  !> is a pure function of the global sizes).
  subroutine ghosts_of(r, list, n)
    integer, intent(in) :: r
    integer, allocatable, intent(out) :: list(:)
    integer, intent(out) :: n
    logical, allocatable :: mark(:)
    integer :: g, i, nb(4), nn, lo, hi
    allocate(mark(ncol_g)); mark = .false.
    lo = (r * ncol_g) / nranks + 1
    hi = ((r + 1) * ncol_g) / nranks
    do g = lo, hi
      call neighbours(g, nb, nn)
      do i = 1, nn
        if (nb(i) < lo .or. nb(i) > hi) mark(nb(i)) = .true.
      end do
    end do
    n = count(mark)
    allocate(list(n))
    n = 0
    do g = 1, ncol_g
      if (mark(g)) then
        n = n + 1; list(n) = g
      end if
    end do
  end subroutine

  subroutine build_mesh()
    integer :: g, k, lc, i, nb(4), nn, q, nq, cnt, h, c, o, nmax
    integer, allocatable :: qghost(:), owners(:)
    integer :: nqg
    integer, allocatable :: fo(:), fn(:), ft(:)
    real(rk) :: dth, dph

    if (n_theta < 1 .or. n_phi < 1 .or. n_depth < 3) call mi_fatal('&mesh: bad sizes')
    ncol_g = n_theta * n_phi
    if (ncol_g < nranks) call mi_fatal('&mesh: fewer surface columns than MPI ranks')
    col_lo = (myrank * ncol_g) / nranks + 1
    col_hi = ((myrank + 1) * ncol_g) / nranks
    n_col_own = col_hi - col_lo + 1
    call ghosts_of(myrank, ghost_cols, n_col_ghost)
    n_own = n_col_own * n_depth
    n_ghost = n_col_ghost * n_depth
    n_tot = n_own + n_ghost

    ! radial node fractions, clustered at the heated surface
    allocate(s_node(0:n_depth))
    do k = 0, n_depth
      if (abs(stretch - 1d0) < 1d-12) then
        s_node(k) = real(k, rk) / n_depth
      else
        s_node(k) = (stretch**k - 1d0) / (stretch**n_depth - 1d0)
      end if
    end do
    r_inner = nose_radius - thickness

    ! cell -> global (theta, phi, layer)
    allocate(c_it(n_tot), c_jp(n_tot), c_k(n_tot))
    do lc = 1, n_col_own + n_col_ghost
      if (lc <= n_col_own) then
        g = col_lo + lc - 1
      else
        g = ghost_cols(lc - n_col_own)
      end if
      do k = 1, n_depth
        c = cell_index(lc, k)
        c_it(c) = (g - 1) / n_phi + 1
        c_jp(c) = g - (c_it(c) - 1) * n_phi
        c_k(c) = k
      end do
    end do

    ! faces: radial within a column; lateral once per pair, owner always owned
    nmax = n_own * 3
    allocate(fo(nmax), fn(nmax), ft(nmax))
    nf = 0
    do lc = 1, n_col_own
      g = col_lo + lc - 1
      call neighbours(g, nb, nn)
      do k = 1, n_depth
        c = cell_index(lc, k)
        if (k < n_depth) call add(c, cell_index(lc, k + 1), 1)
        do i = 1, nn
          h = nb(i)
          if (h >= col_lo .and. h <= col_hi .and. h < g) cycle   ! the other side adds it
          o = cell_index(local_column(h), k)
          if (abs(h - g) == 1 .and. (h - 1) / n_phi == (g - 1) / n_phi) then
            call add(c, o, 3)
          else
            call add(c, o, 2)
          end if
        end do
      end do
    end do
    allocate(f_o(nf), f_n(nf), f_type(nf), f_area(nf), f_dist(nf), f_mdot(nf))
    f_o = fo(1:nf); f_n = fn(1:nf); f_type = ft(1:nf)
    f_mdot = 0

    ! halo plan: neighbour ranks are the owners of my ghost columns
    allocate(owners(n_col_ghost))
    do i = 1, n_col_ghost
      owners(i) = col_owner(ghost_cols(i))
    end do
    n_nbr = 0
    allocate(nbr_rank(nranks))
    do q = 0, nranks - 1
      if (any(owners == q)) then
        n_nbr = n_nbr + 1; nbr_rank(n_nbr) = q
      end if
    end do
    allocate(snd_ptr(n_nbr + 1), rcv_ptr(n_nbr + 1))
    allocate(snd_idx(n_ghost * 4 + 1), rcv_idx(n_ghost + 1))
    snd_ptr(1) = 1; rcv_ptr(1) = 1
    cnt = 0
    do nq = 1, n_nbr
      q = nbr_rank(nq)
      do i = 1, n_col_ghost
        if (owners(i) /= q) cycle
        do k = 1, n_depth
          cnt = cnt + 1; rcv_idx(cnt) = cell_index(n_col_own + i, k)
        end do
      end do
      rcv_ptr(nq + 1) = cnt + 1
    end do
    cnt = 0
    do nq = 1, n_nbr
      q = nbr_rank(nq)
      call ghosts_of(q, qghost, nqg)
      do i = 1, nqg
        g = qghost(i)
        if (g < col_lo .or. g > col_hi) cycle
        do k = 1, n_depth
          cnt = cnt + 1
          if (cnt > size(snd_idx)) call mi_fatal('halo send list overflow')
          snd_idx(cnt) = cell_index(g - col_lo + 1, k)
        end do
      end do
      snd_ptr(nq + 1) = cnt + 1
    end do

    ! fields
    allocate(temp(n_tot), pres(n_tot), rho_s(n_tot), r_out(n_tot))
    allocate(rho_i(3, n_own))
    allocate(c_vol(n_tot), c_rmid(n_tot), c_r1(n_tot), c_r2(n_tot))
    ns = n_col_own
    allocate(s_cell(ns), s_area(ns), s_theta(ns), s_phi(ns), s_dhalf(ns))
    allocate(s_ch0(ns), s_ch(ns), s_hr(ns), s_pe(ns), s_qrad(ns))
    allocate(s_tw(ns), s_mg(ns), s_mc(ns), s_rec(ns), s_qcond(ns), s_bc(ns))
    dth = theta_max_deg * pi / 180d0 / n_theta
    dph = phi_span_deg * pi / 180d0 / n_phi
    do lc = 1, ns
      s_cell(lc) = cell_index(lc, 1)
      s_theta(lc) = (c_it(s_cell(lc)) - 0.5d0) * dth
      s_phi(lc) = (c_jp(s_cell(lc)) - 0.5d0) * dph
    end do
    r_out = nose_radius

  contains
    subroutine add(a, b, t)
      integer, intent(in) :: a, b, t
      nf = nf + 1
      if (nf > nmax) call mi_fatal('face list overflow')
      fo(nf) = a; fn(nf) = b; ft(nf) = t
    end subroutine
  end subroutine

  !> Recompute volumes, face areas and distances from the current outer
  !> radius of every column (columns contract as the surface recedes).
  subroutine update_geometry()
    integer :: c, f, o, n, s
    real(rk) :: dth, dph, ta, tb, ra, rb, rm, span
    call halo(r_out)
    dth = theta_max_deg * pi / 180d0 / n_theta
    dph = phi_span_deg * pi / 180d0 / n_phi
    do c = 1, n_tot
      span = r_out(c) - r_inner
      c_r1(c) = r_out(c) - s_node(c_k(c) - 1) * span
      c_r2(c) = r_out(c) - s_node(c_k(c)) * span
      c_rmid(c) = 0.5d0 * (c_r1(c) + c_r2(c))
      ta = (c_it(c) - 1) * dth; tb = c_it(c) * dth
      c_vol(c) = (c_r1(c)**3 - c_r2(c)**3) / 3d0 * (cos(ta) - cos(tb)) * dph
    end do
    do f = 1, nf
      o = f_o(f); n = f_n(f)
      select case (f_type(f))
      case (1)
        ta = (c_it(o) - 1) * dth; tb = c_it(o) * dth
        f_area(f) = c_r2(o)**2 * (cos(ta) - cos(tb)) * dph
        f_dist(f) = c_rmid(o) - c_rmid(n)
      case (2)
        ra = 0.5d0 * (c_r1(o) + c_r1(n)); rb = 0.5d0 * (c_r2(o) + c_r2(n))
        rm = 0.5d0 * (c_rmid(o) + c_rmid(n))
        f_area(f) = 0.5d0 * (ra**2 - rb**2) * sin(min(c_it(o), c_it(n)) * dth) * dph
        f_dist(f) = rm * dth
      case (3)
        ra = 0.5d0 * (c_r1(o) + c_r1(n)); rb = 0.5d0 * (c_r2(o) + c_r2(n))
        rm = 0.5d0 * (c_rmid(o) + c_rmid(n))
        f_area(f) = 0.5d0 * (ra**2 - rb**2) * dth
        f_dist(f) = rm * sin((c_it(o) - 0.5d0) * dth) * dph
      end select
    end do
    do s = 1, ns
      c = s_cell(s)
      ta = (c_it(c) - 1) * dth; tb = c_it(c) * dth
      s_area(s) = r_out(c)**2 * (cos(ta) - cos(tb)) * dph
      s_dhalf(s) = r_out(c) - c_rmid(c)
    end do
  end subroutine

end module mi_mesh
