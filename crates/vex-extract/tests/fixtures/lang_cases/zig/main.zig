const std = @import("std");
const c = @cImport(@cInclude("stdio.h"));
const util = @import("util.zig");

pub const Point = struct {
    x: i32,
    pub fn len(self: Point) i32 {
        return util.abs(self.x) + helper();
    }
    const Inner = struct {
        fn deep() void {}
    };
};

const Color = enum { red, green };

fn helper() i32 {
    std.debug.print("x", .{});
    return Point.len(.{ .x = 1 });
}

pub fn main() void {
    _ = helper();
}
