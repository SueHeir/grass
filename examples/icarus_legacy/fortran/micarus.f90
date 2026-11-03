!=======================================================================
!  micarus: the stand-alone program.
!
!      mpirun -np 4 ./micarus case.nml
!=======================================================================
program micarus
  use mpi
  use mi_driver
  implicit none
  character(len=256) :: input
  integer :: ierr
  call MPI_Init(ierr)
  call get_command_argument(1, input)
  if (len_trim(input) == 0) input = 'micarus.nml'
  call mi_initialize(MPI_COMM_WORLD, input, '')
  call mi_run()
  call mi_finalize()
  call MPI_Finalize(ierr)
end program micarus
