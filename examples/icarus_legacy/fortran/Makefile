# Stand-alone build of the legacy code, the way it has always been built.
#   make            -> ./micarus
#   mpirun -np 4 ./micarus ../cases/oneway.nml
FC      = mpif90
FFLAGS  ?= -O2 -g -fimplicit-none -Wall -Wno-unused-dummy-argument
OBJS = mi_globals.o mi_parallel.o mi_input.o mi_mesh.o mi_solver.o mi_physics.o \
       mi_output.o mi_driver.o

micarus: $(OBJS) micarus.o
	$(FC) $(FFLAGS) -o $@ $^

%.o: %.f90
	$(FC) $(FFLAGS) -c $<

mi_parallel.o: mi_globals.o
mi_input.o: mi_parallel.o
mi_mesh.o: mi_parallel.o
mi_solver.o: mi_parallel.o
mi_physics.o: mi_solver.o mi_input.o mi_mesh.o
mi_output.o: mi_mesh.o mi_input.o
mi_driver.o: mi_physics.o mi_output.o
micarus.o: mi_driver.o

clean:
	rm -f *.o *.mod micarus
.PHONY: clean
