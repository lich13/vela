import AppKit

// The rows of the tray menu's quota, drawn. `tray_quota.rs` says what each
// row is. A title and a row of periods get an attributed title made of that
// here; whatever else a row says is left as its text. The text Rust gave an
// item stays its title, which is what is read out and searched.

/// A row as Rust tells it.
private struct TrayRow: Decodable {
    /// `title`, `detail` or `plain`.
    let kind: String
    let client: String?
    /// Whose account a title is of, or what the periods of a detail are of.
    let name: String?
    let plan: String?
    let current: Bool?
    let periods: [TrayPeriod]?
}

private struct TrayPeriod: Decodable {
    let label: String
    /// What is left of it, where the service said.
    let left: TrayLeft?
}

private struct TrayLeft: Decodable {
    let percent: Int
    /// How many cells of the bar are filled.
    let filled: Int
    /// `normal`, `orange` or `red`.
    let tone: String
}

/// How many cells a bar has. Every cell is the same character, so that all
/// are as wide: a filled one and an empty one differ in colour alone.
private let trayCells = 12
private let trayCell = "▰"
/// What every row under a title starts with.
private let trayIndent = "    "

private func trayWidth(_ text: String, _ font: NSFont) -> CGFloat {
    NSAttributedString(string: text, attributes: [.font: font]).size().width
}

/// The fonts and the columns the rows are drawn with. The columns are tab
/// stops, three to a period: where its number ends, where its bar starts, and
/// where what follows the bar starts, which is the next period or what the
/// periods are of. They are the same in every row, so that the numbers and
/// the bars stand under each other whatever the periods are called.
private struct TrayLayout {
    /// The menu's own font, in its own size.
    let font: NSFont
    let bold: NSFont
    /// Digits that are all as wide, for the numbers.
    let digits: NSFont
    let columns: NSParagraphStyle
    /// How many periods a row that says what they are of has at most. What
    /// they are of is said after as many, in every such row.
    let named: Int

    init(_ rows: [TrayRow]) {
        let font = NSFont.menuFont(ofSize: 0)
        let digits = NSFont.monospacedDigitSystemFont(ofSize: font.pointSize, weight: .regular)
        let details = rows.filter { $0.kind == "detail" }
        let space = trayWidth(" ", font)
        let label = details.flatMap { $0.periods ?? [] }.map { trayWidth($0.label, font) }.max() ?? 0
        let number = trayWidth("100%", digits)
        let bar = trayWidth(String(repeating: trayCell, count: trayCells), font)
        var stops: [NSTextTab] = []
        var start = trayWidth(trayIndent, font)
        for _ in 0..<(details.map { $0.periods?.count ?? 0 }.max() ?? 0) {
            // What comes after a tab starts with a space or three: see
            // `detail`. Every column has room for them.
            let numberEnds = start + label + space + number
            let barStarts = numberEnds + space
            let nextStarts = barStarts + space + bar + space
            stops.append(NSTextTab(textAlignment: .right, location: numberEnds))
            stops.append(NSTextTab(textAlignment: .left, location: barStarts))
            stops.append(NSTextTab(textAlignment: .left, location: nextStarts))
            start = nextStarts + 3 * space
        }
        let style = NSMutableParagraphStyle()
        style.tabStops = stops
        // A row is one line, and none of it is to be folded away or cut to
        // an ellipsis where it is measured a hair wider than it is drawn.
        style.lineBreakMode = .byClipping
        self.font = font
        bold = NSFont.boldSystemFont(ofSize: font.pointSize)
        self.digits = digits
        columns = style
        named = details.filter { $0.name != nil }.map { $0.periods?.count ?? 0 }.max() ?? 0
    }

    /// What `row` is drawn as, nothing where it is drawn as its text.
    func drawn(_ row: TrayRow) -> NSAttributedString? {
        switch row.kind {
        case "title": return title(row)
        case "detail": return detail(row)
        default: return nil
        }
    }

    /// `client · name（plan） ✓`, the client in bold and the plan less marked
    /// than the rest. A title cannot be chosen, and text that is given no
    /// colour is greyed in such an item: the colours are given, so that the
    /// plan is not the darkest of it.
    private func title(_ row: TrayRow) -> NSAttributedString {
        let text = NSMutableAttributedString()
        func add(_ string: String, _ font: NSFont, _ color: NSColor) {
            text.append(
                NSAttributedString(string: string, attributes: [.font: font, .foregroundColor: color]))
        }
        add(row.client ?? "", bold, .labelColor)
        if let name = row.name {
            add(" · " + name, font, .labelColor)
        }
        if let plan = row.plan {
            add("（" + plan + "）", font, .secondaryLabelColor)
        }
        if row.current == true {
            add(" ✓", font, .labelColor)
        }
        return text
    }

    /// The periods of a row in their columns, and what they are of after
    /// them. A number has no colour of its own, and neither has a filled cell
    /// of a bar that warns of nothing: the menu colours these, and turns them
    /// white in the row that is chosen.
    private func detail(_ row: TrayRow) -> NSAttributedString {
        let text = NSMutableAttributedString()
        func add(_ string: String, _ font: NSFont, _ color: NSColor? = nil) {
            var attributes: [NSAttributedString.Key: Any] = [.font: font]
            attributes[.foregroundColor] = color
            text.append(NSAttributedString(string: string, attributes: attributes))
        }
        // The tab stop the next tab goes to.
        var next = 0
        // Goes on at tab stop `stop`. The spaces come after the tabs, where
        // they move what follows them alike in every row. A menu that made
        // nothing of the tabs would show the row spaced by these alone, and
        // the numbers, padded to one width, would still stand in columns.
        func tab(to stop: Int, _ spaces: String) {
            add(String(repeating: "\t", count: max(0, stop - next + 1)) + spaces, font)
            next = max(next, stop + 1)
        }
        add(trayIndent, font)
        for (slot, period) in (row.periods ?? []).enumerated() {
            if slot > 0 {
                tab(to: 3 * slot - 1, "   ")
            }
            add(period.label, font, .secondaryLabelColor)
            tab(to: 3 * slot, " ")
            guard let left = period.left else {
                add("—", digits, .secondaryLabelColor)
                continue
            }
            let number = "\(left.percent)%"
            add(String(repeating: "\u{2007}", count: max(0, 4 - number.count)) + number, digits)
            tab(to: 3 * slot + 1, " ")
            let filled = min(max(left.filled, 0), trayCells)
            let tone: NSColor? =
                left.tone == "red" ? .systemRed : left.tone == "orange" ? .systemOrange : nil
            add(String(repeating: trayCell, count: filled), font, tone)
            add(String(repeating: trayCell, count: trayCells - filled), font, .tertiaryLabelColor)
        }
        if let name = row.name {
            tab(to: 3 * named - 1, "   ")
            add(name, font, .secondaryLabelColor)
        }
        text.addAttribute(
            .paragraphStyle, value: columns, range: NSRange(location: 0, length: text.length))
        return text
    }
}

/// Draws the items of `menu` from `first` on the way `json` says, a JSON array
/// with one element for each of them in their order. The items after these
/// are not touched. Called on the main thread.
@_cdecl("vela_tray_menu_style")
public func vela_tray_menu_style(
    _ menu: UnsafeRawPointer?, _ first: Int, _ json: UnsafePointer<CChar>?
) {
    guard let menu = menu, let json = json, first >= 0,
        let rows = try? JSONDecoder().decode(
            [TrayRow].self, from: Data(bytes: json, count: strlen(json)))
    else { return }
    let items = Unmanaged<NSMenu>.fromOpaque(menu).takeUnretainedValue()
    let layout = TrayLayout(rows)
    for (offset, row) in rows.enumerated() {
        guard first + offset < items.numberOfItems, let item = items.item(at: first + offset)
        else { return }
        let drawn = layout.drawn(row)
        // An item that is drawn as it is to be is left alone, so that a menu
        // that is open draws only what changed.
        if let drawn = drawn {
            if item.attributedTitle?.isEqual(to: drawn) == true { continue }
        } else if item.attributedTitle == nil {
            continue
        }
        // Giving an item an attributed title makes its characters the title
        // as well, and taking it away leaves them there.
        let title = item.title
        item.attributedTitle = drawn
        if item.title != title {
            item.title = title
        }
    }
}
