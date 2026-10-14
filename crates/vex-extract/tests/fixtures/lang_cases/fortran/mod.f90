module Geometry
  use iso_fortran_env
  implicit none
  type :: Point
    real :: x, y
  end type Point
contains
  function Dist(a, b) result(d)
    type(Point) :: a, b
    real :: d
    d = sqrt((a%x - b%x)**2) + Square(a%x)
  end function Dist
  real function Square(v)
    real :: v
    Square = v * v
  end function Square
  subroutine Report(p)
    type(Point) :: p
    call Show(p)
    call external_sub()
  end subroutine
end module Geometry

program Main
  use Geometry
  call Report(Point(1.0, 2.0))
end program Main
