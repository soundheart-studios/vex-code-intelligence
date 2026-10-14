module Shapes
using LinearAlgebra, .Local
import Base: show, +
import ..Parent.Mod

abstract type Shape end
struct Circle{T} <: Shape
    r::T
end
primitive type Word 32 end

function area(c::Circle)::Float64
    return pi * c.r^2 + helper(c)
end
function Base.show(io::IO, c::Circle) where {T}
    print(io, "Circle")
end
Base.:(==)(a::Circle, b::Circle) = a.r == b.r
Base.:+(a::Circle, b::Circle) = Circle(a.r + b.r)
helper(c) = sqrt(c.r)
macro twice(ex)
    return :($ex; $ex)
end
function outer()
    inner() = 1
    @twice inner()
end
end
