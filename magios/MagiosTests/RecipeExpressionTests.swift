import XCTest
@testable import Magician

final class RecipeExpressionTests: XCTestCase {
    private func e(_ s: String, _ env: [String: Double] = [:]) -> Double? {
        RecipeExpression.evaluate(s) { env[$0] }
    }

    func testNumberLiteralsAndArithmetic() {
        XCTAssertEqual(e("28"), 28)
        XCTAssertEqual(e("2+3"), 5)
        XCTAssertEqual(e("10-4"), 6)
        XCTAssertEqual(e("6*7"), 42)
        XCTAssertEqual(e("20/4"), 5)
        XCTAssertEqual(e("1.5+2.5"), 4)
    }

    func testPrecedenceAndParentheses() {
        XCTAssertEqual(e("2+3*4"), 14)          // * binds tighter than +
        XCTAssertEqual(e("(2+3)*4"), 20)
        XCTAssertEqual(e("10-2-3"), 5)          // left assoc
        XCTAssertEqual(e("20/4/5"), 1)          // left assoc
    }

    func testUnaryMinus() {
        XCTAssertEqual(e("-5"), -5)
        XCTAssertEqual(e("3+-2"), 1)
        XCTAssertEqual(e("-(2+3)"), -5)
    }

    func testIdentifierResolution() {
        XCTAssertEqual(e("x+size", ["x": 10, "size": 28]), 38)
        XCTAssertEqual(e("r*0.5", ["r": 40]), 20)
        XCTAssertNil(e("missing", [:]))          // undefined outside a coalesce → nil
        XCTAssertNil(e("x+missing", ["x": 10]))  // undefined operand poisons the term
    }

    func testCoalesceFirstDefined() {
        XCTAssertEqual(e("cx|x", ["x": 5]), 5)               // cx undefined → x
        XCTAssertEqual(e("cx|x", ["cx": 9, "x": 5]), 9)      // cx defined → cx
        XCTAssertEqual(e("a|b|c", ["c": 3]), 3)              // fall through
        XCTAssertNil(e("a|b", [:]))                          // none defined → nil
    }

    func testCoalesceIsLowestPrecedence() {
        // Parsed as (r) | (size) — not r|(size)… but with only size defined we get size.
        XCTAssertEqual(e("r|size", ["size": 36]), 36)
        // Additive binds tighter than coalesce: (x+1)|(y) with x defined.
        XCTAssertEqual(e("x+1|y", ["x": 4]), 5)
    }

    func testFunctions() {
        XCTAssertEqual(e("sqrt(9)"), 3)
        XCTAssertEqual(e("abs(-7)"), 7)
        XCTAssertEqual(e("min(3,8)"), 3)
        XCTAssertEqual(e("max(3,8)"), 8)
        XCTAssertEqual(e("min(260,300)"), 260)      // callout bg-width clamp shape
        // deg = degrees→radians; cos(deg(0)) == 1
        XCTAssertEqual(e("cos(deg(0))")!, 1, accuracy: 1e-9)
        XCTAssertEqual(e("sin(deg(90))")!, 1, accuracy: 1e-9)
        // deg(180) == pi
        XCTAssertEqual(e("deg(180)")!, Double.pi, accuracy: 1e-9)
        // rad = radians→degrees
        XCTAssertEqual(e("rad(deg(90))")!, 90, accuracy: 1e-9)
    }

    func testNestedFunctionExpression() {
        // cos(deg(start_angle))*r
        let v = e("cos(deg(a))*r", ["a": 0, "r": 36])
        XCTAssertEqual(v!, 36, accuracy: 1e-9)
    }

    func testDivisionAndText_lenShapedExpression() {
        // callout width: min(260, text_len*9+16)
        XCTAssertEqual(e("min(260,text_len*9+16)", ["text_len": 4]), 52)
        XCTAssertEqual(e("min(260,text_len*9+16)", ["text_len": 100]), 260)
    }

    func testMalformedExpressionReturnsNil() {
        XCTAssertNil(e("2+"))
        XCTAssertNil(e("(3"))
        XCTAssertNil(e("*4"))
        XCTAssertNil(e("sqrt()"))
        XCTAssertNil(e(""))
    }

    func testSqrtOfNegativeIsNil() {
        XCTAssertNil(e("sqrt(-1)"))
    }
}
