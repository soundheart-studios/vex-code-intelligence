import Foundation
import UIKit.UIView

protocol Drawable {
    func draw()
    var size: Int { get }
}

class Canvas: Drawable {
    var size: Int = 0
    func draw() { render(size) }
    class Nested { func inner() { Canvas().draw() } }
}

extension Canvas {
    @discardableResult
    func clear() -> Bool { return reset() }
}

struct Plain { func noop() {} }

func render(_ n: Int) { print(n) }
