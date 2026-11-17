import AppKit
import SwiftUI

// MARK: - ViewModel Contract (Derived 100% in Rust quick_macos.rs)

struct FreezeStageModel: Codable, Equatable, Identifiable {
    var id: String { name }
    let name: String
    let count: Int
}

struct FreezeCardModel: Codable, Equatable {
    let tone: String
    let label: String
    let buttonLabel: String
    let frozen: Bool
    let disabled: Bool
    let stages: [FreezeStageModel]
    let error: String?
}

struct ClientTabModel: Codable, Equatable, Identifiable {
    let id: String
    let label: String
    let selected: Bool
}

struct LongWaitModel: Codable, Equatable, Identifiable {
    var id: String { requestId }
    let requestId: String
    let providerName: String
    let model: String
    let startedAtText: String
    let phaseText: String
    let timingText: String
    let cancelling: Bool
}

struct QuotaPlanRowModel: Codable, Equatable, Identifiable {
    let id: Int
    let name: String
    let resetText: String?
    let resetTitle: String?
    let remainingText: String
    let usedText: String
    let totalText: String
}

struct QuotaSubscriptionDetailModel: Codable, Equatable, Identifiable {
    let id: Int
    let title: String
    let statusBadge: String?
    let expiresText: String?
    let unitBadge: String?
    let plans: [QuotaPlanRowModel]
}

struct QuotaUsageRowModel: Codable, Equatable, Identifiable {
    var id: String { label }
    let label: String
    let requests: String
    let tokens: String
    let cost: String
}

struct QuotaDetailModel: Codable, Equatable {
    let keyOnlyBanner: String?
    let headline: String
    let sourceLabel: String?
    let updatedTime: String?
    let updatedTitle: String?
    let keyWarning: String?
    let errorText: String?
    let subscriptions: [QuotaSubscriptionDetailModel]
    let standaloneUnit: String?
    let standalonePlans: [QuotaPlanRowModel]
    let expiryText: String?
    let usageRows: [QuotaUsageRowModel]
}

struct QuotaCardModel: Codable, Equatable {
    let tone: String
    let subscriptionLabel: String?
    let compactValue: String
    let headline: String
    let refreshDisabled: Bool
    let loading: Bool
    let hasError: Bool
    let refreshTitle: String
    let detail: QuotaDetailModel
}

struct ProviderCardModel: Codable, Equatable, Identifiable {
    let id: String
    let name: String
    let priority: String?
    let selected: Bool
    let selectLabel: String
    let settingsLabel: String
    let handoffStatus: String?
    let runtimeAlert: String?
    let faultEventId: String?
    let faultProviderId: String?
    let showCodexPolicies: Bool
    let handoffAfterCompaction: Bool
    let takeNewThreads: Bool
    let concurrencyLabel: String
    let maxConcurrency: UInt32
    let rpmLabel: String
    let maxRpm: UInt32
    let queued: Bool
    let queueLabel: String
    let canMoveUp: Bool
    let canMoveDown: Bool
    let quota: QuotaCardModel
}

struct GatewayCardModel: Codable, Equatable {
    let clientId: String
    let revision: String
    let running: Bool
    let statusLabel: String
    let mode: String
    let modeLabel: String
    let powerDisabled: Bool
    let powerLabel: String
    let waitingBadge: String?
    let longWaits: [LongWaitModel]
    let providers: [ProviderCardModel]
}

struct ControlsCardModel: Codable, Equatable {
    let powerError: String?
    let showClamshell: Bool
    let clamshellEnabled: Bool
    let clamshellDisabled: Bool
    let clamshellReadError: String?
    let quitBusy: String?
}

struct QuickPanelViewModel: Codable, Equatable {
    let appearanceMode: String
    let pinned: Bool
    let busy: Bool
    let freeze: FreezeCardModel
    let clients: [ClientTabModel]
    let selectedClient: String?
    let clientVisible: Bool
    let errorBanner: String?
    let noticeBanner: String?
    let gateway: GatewayCardModel?
    let controls: ControlsCardModel
    let limitError: String?
    let limitCloseSeq: UInt64
    let renameCloseSeq: UInt64

    static let placeholder = QuickPanelViewModel(
        appearanceMode: "system",
        pinned: false,
        busy: false,
        freeze: FreezeCardModel(
            tone: "neutral",
            label: "读取状态",
            buttonLabel: "全局冻结",
            frozen: false,
            disabled: true,
            stages: [],
            error: nil
        ),
        clients: [],
        selectedClient: nil,
        clientVisible: true,
        errorBanner: nil,
        noticeBanner: nil,
        gateway: nil,
        controls: ControlsCardModel(
            powerError: nil,
            showClamshell: false,
            clamshellEnabled: false,
            clamshellDisabled: true,
            clamshellReadError: nil,
            quitBusy: nil
        ),
        limitError: nil,
        limitCloseSeq: 0,
        renameCloseSeq: 0
    )
}

// MARK: - Design Tokens (Matching src/styles/tokens.css)

struct QuickPalette {
    let isDark: Bool

    var canvas: Color { isDark ? Self.hex(0x070B14) : Self.hex(0xF3F6FC) }
    var surface: Color { isDark ? Self.hex(0x0E1525) : Self.hex(0xFFFFFF) }
    var surfaceSubtle: Color { isDark ? Self.hex(0x121B2F) : Self.hex(0xEDF2FA) }
    var elevated: Color { isDark ? Self.hex(0x17233B) : Self.hex(0xE5ECF7) }
    var ink: Color { isDark ? Self.hex(0xECF2FF) : Self.hex(0x0F172A) }
    var muted: Color { isDark ? Self.hex(0x8EA1C2) : Self.hex(0x526480) }
    var line: Color { isDark ? Self.rgba(148, 174, 219, 0.145) : Self.rgba(15, 23, 42, 0.095) }
    var lineStrong: Color { isDark ? Self.rgba(148, 174, 219, 0.26) : Self.rgba(15, 23, 42, 0.18) }

    var accent: Color { isDark ? Self.hex(0x38BDF8) : Self.hex(0x0284C7) }
    var accentBlue: Color { isDark ? Self.hex(0x60A5FA) : Self.hex(0x2563EB) }
    var accentAmber: Color { isDark ? Self.hex(0xFBBF24) : Self.hex(0xB45309) }
    var accentEmerald: Color { isDark ? Self.hex(0x34D399) : Self.hex(0x059669) }
    var accentSoft: Color { isDark ? Self.rgba(56, 189, 248, 0.14) : Self.rgba(2, 132, 199, 0.11) }

    var success: Color { isDark ? Self.hex(0x34D399) : Self.hex(0x059669) }
    var warning: Color { isDark ? Self.hex(0xFBBF24) : Self.hex(0xB45309) }
    var warningSoft: Color { isDark ? Self.rgba(251, 191, 36, 0.15) : Self.rgba(217, 119, 6, 0.11) }
    var error: Color { isDark ? Self.hex(0xFB7185) : Self.hex(0xE11D48) }
    var errorSoft: Color { isDark ? Self.rgba(251, 113, 133, 0.15) : Self.rgba(225, 29, 72, 0.10) }
    var iceAccent: Color { isDark ? Self.hex(0x7DD3FC) : Self.hex(0x0284C7) }
    var iceBorder: Color { isDark ? Self.rgba(125, 211, 252, 0.46) : Self.rgba(56, 189, 248, 0.48) }
    var onPrimary: Color { isDark ? Self.hex(0x051322) : Self.hex(0xFFFFFF) }

    func clientColor(_ id: String) -> Color {
        switch id {
        case "claude": return accentBlue
        case "grok": return accentAmber
        case "dsh": return accentEmerald
        default: return accent
        }
    }

    func quotaColor(_ tone: String) -> Color {
        switch tone {
        case "good": return success
        case "warn": return warning
        case "critical": return error
        case "normal": return ink
        default: return muted
        }
    }

    static func hex(_ value: UInt32, alpha: Double = 1.0) -> Color {
        let r = Double((value >> 16) & 0xFF) / 255.0
        let g = Double((value >> 8) & 0xFF) / 255.0
        let b = Double(value & 0xFF) / 255.0
        return Color(.sRGB, red: r, green: g, blue: b, opacity: alpha)
    }

    static func rgba(_ r: Double, _ g: Double, _ b: Double, _ a: Double) -> Color {
        Color(.sRGB, red: r / 255.0, green: g / 255.0, blue: b / 255.0, opacity: a)
    }
}

// MARK: - Observable Store & Ephemeral Overlay State

struct LimitPopoverDraft: Equatable {
    let providerId: String
    let providerName: String
    let kind: String // "concurrency" | "rpm"
    var value: String
    var expectedRevision: String?
}

struct DragSession: Equatable {
    let id: String
    let initialIds: [String]
    let expectedRevision: String
    var currentIds: [String]
    var offsetY: CGFloat
}

/// Row heights as laid out, keyed by provider id, for drag targets.
struct ProviderRowHeightKey: PreferenceKey {
    static var defaultValue: [String: CGFloat] = [:]
    static func reduce(value: inout [String: CGFloat], nextValue: () -> [String: CGFloat]) {
        value.merge(nextValue(), uniquingKeysWith: { $1 })
    }
}

final class QuickPanelStore: ObservableObject {
    /// Not published: a height change must not redraw the list by itself.
    var rowHeights: [String: CGFloat] = [:]
    /// A rename was sent and its outcome has not arrived yet.
    var renamePending: Bool = false
    @Published var decodeFailed: Bool = false
    @Published var model: QuickPanelViewModel = .placeholder
    @Published var editingProviderId: String? = nil
    @Published var editingNameDraft: String = ""
    @Published var editingRevision: String? = nil
    @Published var limitPopover: LimitPopoverDraft? = nil
    @Published var quotaDetailProviderId: String? = nil
    @Published var quotaUsageExpanded: Bool = false
    @Published var activeDrag: DragSession? = nil

    var sendAction: (([String: Any]) -> Void)?

    func apply(_ next: QuickPanelViewModel) {
        let clientChanged = model.selectedClient != next.selectedClient
        let prevLimitClose = model.limitCloseSeq
        let prevRenameClose = model.renameCloseSeq
        let prevError = model.limitError
        let prevBanner = model.errorBanner

        model = next

        if clientChanged {
            editingProviderId = nil
            limitPopover = nil
            quotaDetailProviderId = nil
            activeDrag = nil
        }
        if next.limitCloseSeq != prevLimitClose {
            limitPopover = nil
        } else if limitPopover != nil && next.limitError != nil && next.limitError != prevError {
            limitPopover?.expectedRevision = nil
        }
        if next.renameCloseSeq != prevRenameClose {
            editingProviderId = nil
            renamePending = false
        } else if renamePending && editingProviderId != nil
            && next.errorBanner != nil && next.errorBanner != prevBanner {
            // Only a failed submit drops the baseline, so a retry uses the new revision.
            editingRevision = nil
            renamePending = false
        }
        if let drag = activeDrag, let gw = next.gateway, drag.expectedRevision != gw.revision {
            activeDrag = nil
        }
        if let qid = quotaDetailProviderId,
           next.gateway?.providers.contains(where: { $0.id == qid }) != true {
            quotaDetailProviderId = nil
        }
    }

    func handleEscape() -> Bool {
        if activeDrag != nil {
            activeDrag = nil
            return true
        }
        if quotaDetailProviderId != nil {
            quotaDetailProviderId = nil
            return true
        }
        if limitPopover != nil {
            limitPopover = nil
            sendAction?(["op": "clearLimitError"])
            return true
        }
        if editingProviderId != nil {
            editingProviderId = nil
            return true
        }
        return false
    }

    func resetTransient() {
        activeDrag = nil
        quotaDetailProviderId = nil
        limitPopover = nil
        editingProviderId = nil
    }

    func dispatch(_ payload: [String: Any]) {
        sendAction?(payload)
    }
}

// MARK: - AppKit NSVisualEffectView Wrapper

struct VisualEffectSurface: NSViewRepresentable {
    let isDark: Bool

    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.blendingMode = .behindWindow
        view.state = .active
        view.material = isDark ? .hudWindow : .popover
        return view
    }

    func updateNSView(_ view: NSVisualEffectView, context: Context) {
        view.material = isDark ? .hudWindow : .popover
        view.state = .active
    }
}

struct ConnectionTestPulseIcon: View {
    let color: Color

    var body: some View {
        GeometryReader { geo in
            let w = geo.size.width
            let h = geo.size.height
            let sx = w / 24.0
            let sy = h / 24.0
            ZStack {
                Path { p in
                    p.move(to: CGPoint(x: 5 * sx, y: 12 * sy))
                    p.addLine(to: CGPoint(x: 8 * sx, y: 12 * sy))
                    p.addLine(to: CGPoint(x: 11 * sx, y: 6 * sy))
                    p.addLine(to: CGPoint(x: 14 * sx, y: 18 * sy))
                    p.addLine(to: CGPoint(x: 17 * sx, y: 12 * sy))
                    p.addLine(to: CGPoint(x: 19 * sx, y: 12 * sy))
                }
                .stroke(color, style: StrokeStyle(lineWidth: 1.8, lineCap: .round, lineJoin: .round))
                Circle()
                    .strokeBorder(color, lineWidth: 1.6)
                    .frame(width: 3.6 * sx, height: 3.6 * sy)
                    .position(x: 3 * sx, y: 12 * sy)
                Circle()
                    .strokeBorder(color, lineWidth: 1.6)
                    .frame(width: 3.6 * sx, height: 3.6 * sy)
                    .position(x: 21 * sx, y: 12 * sy)
            }
        }
        .frame(width: 14, height: 14)
    }
}

// MARK: - Freeze Card & Header Toolbar

struct QuickFreezeCardView: View {
    let freeze: FreezeCardModel
    let palette: QuickPalette
    let onToggle: () -> Void

    private var toneColor: Color {
        switch freeze.tone {
        case "red": return palette.error
        case "yellow": return palette.warning
        case "green": return palette.success
        case "ice": return palette.iceAccent
        default: return palette.muted
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                HStack(spacing: 8) {
                    Circle()
                        .fill(toneColor)
                        .frame(width: 9, height: 9)
                        .shadow(color: toneColor.opacity(0.55), radius: 4, x: 0, y: 0)
                    Text(freeze.label)
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundColor(toneColor)
                        .lineLimit(1)
                }
                Spacer(minLength: 6)
                Button(action: onToggle) {
                    HStack(spacing: 5) {
                        Image(systemName: freeze.frozen ? "play.fill" : "pause.fill")
                            .font(.system(size: 11, weight: .semibold))
                        Text(freeze.buttonLabel)
                            .font(.system(size: 12.5, weight: .semibold))
                    }
                    .foregroundColor(freeze.frozen ? palette.onPrimary : palette.ink)
                    .padding(.horizontal, 13)
                    .frame(minWidth: 96, minHeight: 31)
                    .background(
                        Capsule()
                            .fill(freeze.frozen ? palette.accent : palette.surfaceSubtle)
                    )
                    .overlay(
                        Capsule()
                            .strokeBorder(freeze.frozen ? Color.clear : palette.lineStrong, lineWidth: 1)
                    )
                }
                .buttonStyle(.plain)
                .disabled(freeze.disabled)
                .opacity(freeze.disabled ? 0.52 : 1.0)
            }

            if !freeze.stages.isEmpty {
                VStack(alignment: .leading, spacing: 5) {
                    ForEach(Array(stageRows.enumerated()), id: \.offset) { _, row in
                        HStack(spacing: 6) {
                            ForEach(row) { stage in
                                HStack(spacing: 4) {
                                    Text(stage.name)
                                        .foregroundColor(palette.muted)
                                        .lineLimit(1)
                                    Text("\(stage.count)")
                                        .font(.system(size: 12, weight: .semibold).monospacedDigit())
                                        .foregroundColor(palette.ink)
                                        .lineLimit(1)
                                }
                                .font(.system(size: 12))
                                .padding(.horizontal, 8)
                                .padding(.vertical, 2)
                                .background(Capsule().fill(palette.surfaceSubtle))
                                .fixedSize(horizontal: true, vertical: false)
                            }
                        }
                    }
                }
            }

            if let err = freeze.error, !err.isEmpty {
                Text(err)
                    .font(.system(size: 12))
                    .foregroundColor(palette.error)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.horizontal, 13)
        .padding(.vertical, 10)
        .background(
            ZStack {
                RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .fill(palette.surface)
                RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .fill(
                        LinearGradient(
                            gradient: Gradient(colors: [
                                toneColor.opacity(freeze.tone == "ice" ? 0.18 : 0.09),
                                Color.clear
                            ]),
                            startPoint: .topLeading,
                            endPoint: .bottomTrailing
                        )
                    )
            }
        )
        .overlay(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .strokeBorder(
                    freeze.tone == "ice" ? palette.iceBorder : toneColor.opacity(0.28),
                    lineWidth: freeze.tone == "ice" ? 1.3 : 1.0
                )
        )
        .shadow(
            color: freeze.tone == "ice" ? palette.iceAccent.opacity(0.18) : Color.black.opacity(0.08),
            radius: freeze.tone == "ice" ? 10 : 4,
            x: 0,
            y: 2
        )
    }

    private var stageRows: [[FreezeStageModel]] {
        let maxRowWidth: CGFloat = 368.0
        var rows: [[FreezeStageModel]] = []
        var current: [FreezeStageModel] = []
        var currentWidth: CGFloat = 0
        for stage in freeze.stages {
            let chipWidth = CGFloat(stage.name.count) * 12.5
                + CGFloat(String(stage.count).count) * 7.5
                + 22.0
            let nextWidth = current.isEmpty ? chipWidth : (currentWidth + 6.0 + chipWidth)
            if !current.isEmpty && nextWidth > maxRowWidth {
                rows.append(current)
                current = [stage]
                currentWidth = chipWidth
            } else {
                current.append(stage)
                currentWidth = nextWidth
            }
        }
        if !current.isEmpty {
            rows.append(current)
        }
        return rows
    }
}

struct QuickToolbarView: View {
    let clients: [ClientTabModel]
    let pinned: Bool
    let busy: Bool
    let palette: QuickPalette
    let onSelectClient: (String) -> Void
    let onTogglePin: () -> Void
    let onClose: () -> Void

    var body: some View {
        HStack(spacing: 6) {
            if !clients.isEmpty {
                HStack(spacing: 2) {
                    ForEach(clients) { tab in
                        Button(action: { onSelectClient(tab.id) }) {
                            Text(tab.label)
                                .font(.system(size: 12.5, weight: tab.selected ? .semibold : .medium))
                                .foregroundColor(tab.selected ? palette.clientColor(tab.id) : palette.muted)
                                .lineLimit(1)
                                .frame(maxWidth: .infinity)
                                .padding(.vertical, 5)
                                .padding(.horizontal, 6)
                                .background(
                                    Capsule()
                                        .fill(tab.selected ? palette.surface : Color.clear)
                                )
                                .overlay(
                                    Capsule()
                                        .strokeBorder(tab.selected ? palette.line : Color.clear, lineWidth: 1)
                                )
                        }
                        .buttonStyle(.plain)
                        .disabled(busy)
                    }
                }
                .padding(2)
                .background(Capsule().fill(palette.surfaceSubtle))
            } else {
                Spacer(minLength: 0)
            }

            Button(action: onTogglePin) {
                Image(systemName: pinned ? "pin.fill" : "pin")
                    .font(.system(size: 12.5, weight: .medium))
                    .foregroundColor(pinned ? palette.accent : palette.muted)
                    .frame(width: 28, height: 28)
                    .background(Capsule().fill(pinned ? palette.accentSoft : Color.clear))
            }
            .buttonStyle(.plain)
            .help(pinned ? "取消固定面板" : "固定面板")

            Button(action: onClose) {
                Image(systemName: "xmark")
                    .font(.system(size: 12.5, weight: .semibold))
                    .foregroundColor(palette.muted)
                    .frame(width: 28, height: 28)
            }
            .buttonStyle(.plain)
            .help("关闭快捷面板")
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 4)
        .frame(minHeight: 38)
        .background(Capsule().fill(palette.surface))
        .overlay(Capsule().strokeBorder(palette.line, lineWidth: 1))
    }
}

// MARK: - Gateway Bar, Long Waits & Provider Rows

struct QuickGatewayBarView: View {
    let gateway: GatewayCardModel
    let busy: Bool
    let palette: QuickPalette
    let onToggleMode: () -> Void
    let onTogglePower: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            HStack(spacing: 6) {
                Circle()
                    .fill(gateway.running ? palette.success : palette.muted)
                    .frame(width: 7, height: 7)
                Text(gateway.statusLabel)
                    .font(.system(size: 12.5, weight: .semibold))
                    .foregroundColor(gateway.running ? palette.success : palette.muted)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .background(
                Capsule()
                    .fill(gateway.running ? palette.success.opacity(0.13) : palette.surfaceSubtle)
            )
            .overlay(
                Capsule()
                    .strokeBorder(gateway.running ? palette.success.opacity(0.30) : palette.line, lineWidth: 1)
            )

            Spacer(minLength: 4)

            Button(action: onToggleMode) {
                Text(gateway.modeLabel)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundColor(gateway.mode == "auto" ? palette.accent : palette.muted)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 4)
                    .background(
                        Capsule()
                            .fill(gateway.mode == "auto" ? palette.accentSoft : palette.surfaceSubtle)
                    )
                    .overlay(
                        Capsule()
                            .strokeBorder(gateway.mode == "auto" ? palette.accent.opacity(0.34) : palette.line, lineWidth: 1)
                    )
            }
            .buttonStyle(.plain)
            .disabled(busy)

            Button(action: onTogglePower) {
                Image(systemName: "power")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundColor(gateway.running ? palette.accent : palette.ink)
                    .frame(width: 28, height: 28)
                    .background(Capsule().fill(palette.surfaceSubtle))
                    .overlay(Capsule().strokeBorder(palette.line, lineWidth: 1))
            }
            .buttonStyle(.plain)
            .disabled(busy || gateway.powerDisabled)
            .opacity(busy || gateway.powerDisabled ? 0.45 : 1.0)
            .help(gateway.powerLabel)
        }
        .padding(.horizontal, 11)
        .padding(.vertical, 7)
        .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(palette.surface))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(palette.line, lineWidth: 1))
    }
}

struct QuickLongWaitsView: View {
    let waits: [LongWaitModel]
    let palette: QuickPalette
    let onCancel: (String) -> Void

    var body: some View {
        VStack(spacing: 0) {
            ForEach(Array(waits.enumerated()), id: \.element.id) { idx, item in
                VStack(alignment: .leading, spacing: 5) {
                    HStack(spacing: 8) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(item.providerName)
                                .font(.system(size: 13, weight: .semibold))
                                .foregroundColor(palette.ink)
                                .lineLimit(1)
                            Text(item.model)
                                .font(.system(size: 12))
                                .foregroundColor(palette.muted)
                                .lineLimit(1)
                        }
                        Spacer(minLength: 4)
                        Button(action: { onCancel(item.requestId) }) {
                            Text(item.cancelling ? "中止中…" : "中止")
                                .font(.system(size: 12, weight: .medium))
                                .foregroundColor(palette.ink)
                                .padding(.horizontal, 11)
                                .frame(height: 28)
                                .background(Capsule().fill(palette.surfaceSubtle))
                                .overlay(Capsule().strokeBorder(palette.line, lineWidth: 1))
                        }
                        .buttonStyle(.plain)
                        .disabled(item.cancelling)
                    }
                    HStack(spacing: 8) {
                        Text(item.startedAtText)
                            .font(.system(size: 12).monospacedDigit())
                        Spacer(minLength: 4)
                        Text(item.phaseText)
                            .font(.system(size: 12))
                        Spacer(minLength: 4)
                        Text(item.timingText)
                            .font(.system(size: 12).monospacedDigit())
                    }
                    .foregroundColor(palette.muted)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 9)
                if idx + 1 < waits.count {
                    Rectangle().fill(palette.line).frame(height: 1)
                }
            }
        }
        .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(palette.surface))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(palette.line, lineWidth: 1))
    }
}

struct QuickProviderRowView: View {
    @ObservedObject var store: QuickPanelStore
    let provider: ProviderCardModel
    let gatewayRevision: String
    let clientId: String
    let rowDisabled: Bool
    let isDragging: Bool
    let palette: QuickPalette
    let onDragChanged: (CGFloat) -> Void
    let onDragEnded: () -> Void
    let onMoveByStep: (Int) -> Void

    @State private var nameHovered: Bool = false
    @FocusState private var nameFocused: Bool

    private var isEditingName: Bool {
        store.editingProviderId == provider.id
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Row 1: Main controls
            HStack(spacing: 4) {
                Image(systemName: "line.3.horizontal")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundColor(palette.muted)
                    .frame(width: 18, height: 24)
                    .contentShape(Rectangle())
                    .gesture(
                        DragGesture(minimumDistance: 4, coordinateSpace: .named("providerList"))
                            .onChanged { value in
                                if !rowDisabled {
                                    onDragChanged(value.translation.height)
                                }
                            }
                            .onEnded { _ in
                                onDragEnded()
                            }
                    )
                    .contextMenu {
                        Button("上移") { onMoveByStep(-1) }
                            .disabled(rowDisabled || !provider.canMoveUp)
                        Button("下移") { onMoveByStep(1) }
                            .disabled(rowDisabled || !provider.canMoveDown)
                    }
                    .help("拖动排序（右键可上移/下移）")

                if let priority = provider.priority {
                    Text(priority)
                        .font(.system(size: 11, weight: .semibold).monospacedDigit())
                        .foregroundColor(palette.accentBlue)
                        .padding(.horizontal, 5)
                        .frame(minWidth: 23, minHeight: 20)
                        .background(Capsule().fill(palette.surfaceSubtle))
                        .fixedSize(horizontal: true, vertical: false)
                }

                if isEditingName {
                    HStack(spacing: 3) {
                        TextField(
                            "名称",
                            text: $store.editingNameDraft,
                            onCommit: { submitRename() }
                        )
                        .textFieldStyle(.plain)
                        .focused($nameFocused)
                        .onAppear { DispatchQueue.main.async { nameFocused = true } }
                        .font(.system(size: 12.5, weight: .medium))
                        .foregroundColor(palette.ink)
                        .padding(.horizontal, 6)
                        .frame(height: 24)
                        .background(
                            RoundedRectangle(cornerRadius: 7, style: .continuous)
                                .fill(palette.surfaceSubtle)
                        )
                        .overlay(
                            RoundedRectangle(cornerRadius: 7, style: .continuous)
                                .strokeBorder(palette.accent, lineWidth: 1)
                        )

                        Button(action: { submitRename() }) {
                            Image(systemName: "checkmark")
                                .font(.system(size: 11, weight: .bold))
                                .foregroundColor(palette.success)
                                .frame(width: 22, height: 22)
                        }
                        .buttonStyle(.plain)
                        .help("保存名称")

                        Button(action: { store.editingProviderId = nil }) {
                            Image(systemName: "xmark")
                                .font(.system(size: 11, weight: .semibold))
                                .foregroundColor(palette.muted)
                                .frame(width: 22, height: 22)
                        }
                        .buttonStyle(.plain)
                        .help("取消重命名")
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                } else {
                    HStack(spacing: 3) {
                        Text(provider.name)
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundColor(palette.ink)
                            .lineLimit(1)
                            .truncationMode(.tail)
                        if nameHovered {
                            Image(systemName: "pencil")
                                .font(.system(size: 10))
                                .foregroundColor(palette.muted.opacity(0.65))
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                    .onHover { nameHovered = $0 }
                    .onTapGesture(count: 2) {
                        if !rowDisabled {
                            store.editingProviderId = provider.id
                            store.renamePending = false
                            store.editingNameDraft = provider.name
                            store.editingRevision = gatewayRevision
                        }
                    }
                    .help("\(provider.name)（双击重命名）")
                }

                // Compact Quota Pill (terse: subscription title stays in tooltip & detail overlay)
                HStack(spacing: 2) {
                    Button(action: {
                        store.quotaUsageExpanded = false
                        store.quotaDetailProviderId = provider.id
                    }) {
                        Text(provider.quota.compactValue)
                            .font(.system(size: 12.5, weight: .semibold).monospacedDigit())
                            .foregroundColor(palette.quotaColor(provider.quota.tone))
                            .lineLimit(1)
                            .padding(.horizontal, 3)
                    }
                    .buttonStyle(.plain)
                    .help(
                        [provider.quota.subscriptionLabel, provider.quota.headline]
                            .compactMap { $0 }
                            .joined(separator: " · ")
                    )

                    Button(action: {
                        store.dispatch(["op": "refreshQuota", "id": provider.id])
                    }) {
                        Image(systemName: "arrow.clockwise")
                            .font(.system(size: 11, weight: .medium))
                            .foregroundColor(provider.quota.hasError ? palette.error : palette.muted)
                            .frame(width: 20, height: 22)
                    }
                    .buttonStyle(.plain)
                    .disabled(provider.quota.refreshDisabled)
                    .opacity(provider.quota.refreshDisabled ? 0.42 : 1.0)
                    .help(provider.quota.refreshTitle)
                }
                .fixedSize(horizontal: true, vertical: false)

                // Select Button
                Button(action: {
                    store.dispatch(["op": "selectProvider", "id": provider.id])
                }) {
                    HStack(spacing: 3) {
                        if provider.selected {
                            Image(systemName: "checkmark")
                                .font(.system(size: 10.5, weight: .bold))
                        }
                        Text(provider.selectLabel)
                            .font(.system(size: 11.5, weight: .semibold))
                            .lineLimit(1)
                    }
                    .foregroundColor(provider.selected ? palette.accent : palette.ink)
                    .padding(.horizontal, 7)
                    .frame(height: 26)
                    .background(Capsule().fill(provider.selected ? palette.accentSoft : palette.surfaceSubtle))
                    .overlay(
                        Capsule()
                            .strokeBorder(provider.selected ? palette.accent.opacity(0.42) : palette.line, lineWidth: 1)
                    )
                }
                .buttonStyle(.plain)
                .disabled(rowDisabled)
                .fixedSize(horizontal: true, vertical: false)

                // Reset Circuit
                iconCircleButton("arrow.counterclockwise", help: "重置熔断") {
                    store.dispatch(["op": "resetProvider", "id": provider.id])
                }

                // Whitelist / Model Mapping -> Main Window
                iconCircleButton("slider.horizontal.3", help: provider.settingsLabel) {
                    store.dispatch([
                        "op": "openMain",
                        "page": "gateway",
                        "providerId": provider.id,
                        "clientId": clientId
                    ])
                }

                // Connection Testing -> Main Window
                Button(action: {
                    store.dispatch([
                        "op": "openMain",
                        "page": "gateway",
                        "providerId": provider.id,
                        "clientId": clientId,
                        "providerAction": "testing"
                    ])
                }) {
                    ConnectionTestPulseIcon(color: palette.muted)
                        .frame(width: 24, height: 24)
                        .background(Capsule().fill(palette.surfaceSubtle.opacity(0.6)))
                }
                .buttonStyle(.plain)
                .disabled(rowDisabled)
                .help("连接测试")
            }

            if let handoff = provider.handoffStatus, !handoff.isEmpty {
                Text(handoff)
                    .font(.system(size: 11.5, weight: .medium))
                    .foregroundColor(palette.accentBlue)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 2)
                    .background(Capsule().fill(palette.accentBlue.opacity(0.13)))
                    .padding(.top, 5)
                    .padding(.leading, 22)
            }

            if let alert = provider.runtimeAlert, !alert.isEmpty {
                Button(action: {
                    var payload: [String: Any] = [
                        "op": "openMain",
                        "page": "logs",
                        "clientId": clientId
                    ]
                    if let eid = provider.faultEventId {
                        payload["eventId"] = eid
                    }
                    if let pid = provider.faultProviderId {
                        payload["providerId"] = pid
                    }
                    store.dispatch(payload)
                }) {
                    Text(alert)
                        .font(.system(size: 11.5, weight: .medium).monospacedDigit())
                        .foregroundColor(palette.warning)
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 3)
                        .background(
                            RoundedRectangle(cornerRadius: 7, style: .continuous)
                                .fill(palette.warningSoft)
                        )
                }
                .buttonStyle(.plain)
                .padding(.top, 5)
                .padding(.leading, 22)
            }

            Rectangle()
                .fill(palette.line)
                .frame(height: 1)
                .padding(.vertical, 7)

            // Row 2: Secondary 5-control bar (nowrap, never clips in 420pt)
            HStack(spacing: 4) {
                if provider.showCodexPolicies {
                    policyChip("压缩后", active: provider.handoffAfterCompaction) {
                        store.dispatch([
                            "op": "toggleProviderPolicy",
                            "id": provider.id,
                            "field": "handoffAfterCompaction"
                        ])
                    }
                    policyChip("新线程", active: provider.takeNewThreads) {
                        store.dispatch([
                            "op": "toggleProviderPolicy",
                            "id": provider.id,
                            "field": "takeNewThreads"
                        ])
                    }
                }

                secondaryChip(
                    title: provider.concurrencyLabel,
                    icon: nil,
                    active: store.limitPopover?.providerId == provider.id && store.limitPopover?.kind == "concurrency"
                ) {
                    openLimit("concurrency", initial: provider.maxConcurrency)
                }

                secondaryChip(
                    title: provider.rpmLabel,
                    icon: "speedometer",
                    active: store.limitPopover?.providerId == provider.id && store.limitPopover?.kind == "rpm"
                ) {
                    openLimit("rpm", initial: provider.maxRpm)
                }

                Spacer(minLength: 0)

                secondaryChip(
                    title: provider.queueLabel,
                    icon: provider.queued ? "checkmark" : "plus",
                    active: provider.queued
                ) {
                    store.dispatch(["op": "toggleProviderQueue", "id": provider.id])
                }
            }
        }
        .padding(.horizontal, 11)
        .padding(.vertical, 9)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(provider.selected ? palette.elevated.opacity(0.65) : palette.surface)
                .background(
                    RoundedRectangle(cornerRadius: 14, style: .continuous)
                        .fill(palette.surface)
                )
        )
        .overlay(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(
                    isDragging
                        ? palette.accent
                        : (provider.selected ? palette.accent.opacity(0.45) : palette.line),
                    lineWidth: isDragging ? 1.4 : 1.0
                )
        )
        .opacity(isDragging ? 0.92 : 1.0)
    }

    private func submitRename() {
        var payload: [String: Any] = [
            "op": "renameProvider",
            "id": provider.id,
            "name": store.editingNameDraft
        ]
        if let rev = store.editingRevision {
            payload["expectedRevision"] = rev
        }
        store.renamePending = true
        store.dispatch(payload)
    }

    private func openLimit(_ kind: String, initial: UInt32) {
        if store.limitPopover?.providerId == provider.id && store.limitPopover?.kind == kind {
            store.limitPopover = nil
            store.dispatch(["op": "clearLimitError"])
        } else {
            store.dispatch(["op": "clearLimitError"])
            store.limitPopover = LimitPopoverDraft(
                providerId: provider.id,
                providerName: provider.name,
                kind: kind,
                value: String(initial),
                expectedRevision: gatewayRevision
            )
        }
    }

    private func iconCircleButton(_ symbol: String, help: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 12, weight: .medium))
                .foregroundColor(palette.muted)
                .frame(width: 24, height: 24)
                .background(Capsule().fill(palette.surfaceSubtle.opacity(0.6)))
        }
        .buttonStyle(.plain)
        .disabled(rowDisabled)
        .help(help)
    }

    private func policyChip(_ label: String, active: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 3.5) {
                Circle()
                    .fill(active ? palette.accent : palette.muted.opacity(0.55))
                    .frame(width: 5.5, height: 5.5)
                Text(label)
                    .font(.system(size: 11.5, weight: .medium))
                    .lineLimit(1)
            }
            .foregroundColor(active ? palette.accent : palette.muted)
            .padding(.horizontal, 6)
            .frame(height: 24)
            .background(Capsule().fill(active ? palette.accentSoft : palette.surfaceSubtle))
            .overlay(Capsule().strokeBorder(active ? palette.accent.opacity(0.35) : palette.line, lineWidth: 1))
        }
        .buttonStyle(.plain)
        .disabled(rowDisabled)
        .fixedSize(horizontal: true, vertical: false)
    }

    private func secondaryChip(title: String, icon: String?, active: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 3.5) {
                if let icon = icon {
                    Image(systemName: icon)
                        .font(.system(size: 10, weight: .semibold))
                }
                Text(title)
                    .font(.system(size: 11.5, weight: .medium).monospacedDigit())
                    .lineLimit(1)
            }
            .foregroundColor(active ? palette.accent : palette.ink)
            .padding(.horizontal, 6.5)
            .frame(height: 24)
            .background(Capsule().fill(active ? palette.accentSoft : palette.surfaceSubtle))
            .overlay(Capsule().strokeBorder(active ? palette.accent.opacity(0.38) : palette.line, lineWidth: 1))
        }
        .buttonStyle(.plain)
        .disabled(rowDisabled)
        .fixedSize(horizontal: true, vertical: false)
    }
}

// MARK: - Controls Dock, Overlays & Root View

struct QuickControlsDockView: View {
    let controls: ControlsCardModel
    let palette: QuickPalette
    let dispatch: ([String: Any]) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let powerErr = controls.powerError, !powerErr.isEmpty {
                HStack(spacing: 6) {
                    Text(powerErr)
                        .font(.system(size: 12))
                        .foregroundColor(palette.error)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 4)
                    Button(action: { dispatch(["op": "dismissPowerError"]) }) {
                        Image(systemName: "xmark")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundColor(palette.error)
                            .frame(width: 20, height: 20)
                    }
                    .buttonStyle(.plain)
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
                .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(palette.errorSoft))
            }

            if controls.showClamshell {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 8) {
                        Image(systemName: "moon")
                            .font(.system(size: 12.5, weight: .medium))
                            .foregroundColor(palette.muted)
                        Text("合盖不休眠")
                            .font(.system(size: 12, weight: .medium))
                            .foregroundColor(palette.ink)
                        Spacer(minLength: 8)
                        Toggle(
                            "",
                            isOn: Binding(
                                get: { controls.clamshellEnabled },
                                set: { _ in dispatch(["op": "toggleClamshell"]) }
                            )
                        )
                        .labelsHidden()
                        .toggleStyle(.switch)
                        .controlSize(.small)
                        .disabled(controls.clamshellDisabled)
                    }
                    .padding(.horizontal, 4)

                    if let readErr = controls.clamshellReadError, !readErr.isEmpty {
                        Text(readErr)
                            .font(.system(size: 11.5))
                            .foregroundColor(palette.error)
                            .padding(.horizontal, 4)
                    }
                }
            }

            HStack(spacing: 7) {
                forceQuitButton(target: "codex", label: "ChatGPT / Codex")
                forceQuitButton(target: "claude", label: "Claude Code")
            }
        }
        .padding(.horizontal, 11)
        .padding(.vertical, 9)
        .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(palette.surface))
        .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).strokeBorder(palette.line, lineWidth: 1))
    }

    private func forceQuitButton(target: String, label: String) -> some View {
        let isBusy = controls.quitBusy == target
        let anyBusy = controls.quitBusy != nil
        return Button(action: {
            dispatch(["op": "forceQuit", "target": target])
        }) {
            HStack(spacing: 6) {
                Image(systemName: "power")
                    .font(.system(size: 11.5, weight: .semibold))
                Text(isBusy ? "正在退出…" : "退出 \(label)")
                    .font(.system(size: 12, weight: .medium))
                    .lineLimit(1)
            }
            .foregroundColor(palette.ink)
            .frame(maxWidth: .infinity, minHeight: 32)
            .padding(.horizontal, 6)
            .background(Capsule().fill(palette.surfaceSubtle))
            .overlay(Capsule().strokeBorder(palette.line, lineWidth: 1))
        }
        .buttonStyle(.plain)
        .disabled(anyBusy)
        .opacity(anyBusy ? 0.55 : 1.0)
    }
}

struct QuickLimitOverlayView: View {
    @ObservedObject var store: QuickPanelStore
    let draft: LimitPopoverDraft
    let palette: QuickPalette

    var body: some View {
        ZStack {
            Color.black.opacity(palette.isDark ? 0.34 : 0.16)
                .contentShape(Rectangle())
                .onTapGesture {
                    store.limitPopover = nil
                    store.dispatch(["op": "clearLimitError"])
                }

            VStack(alignment: .leading, spacing: 10) {
                Text("\(draft.providerName) · \(draft.kind == "rpm" ? "RPM 上限" : "并发上限")")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundColor(palette.ink)
                    .lineLimit(1)

                VStack(alignment: .leading, spacing: 5) {
                    Text("上限（0 不限）")
                        .font(.system(size: 12))
                        .foregroundColor(palette.muted)

                    TextField(
                        "0",
                        text: Binding(
                            get: { store.limitPopover?.value ?? "" },
                            set: { store.limitPopover?.value = $0 }
                        ),
                        onCommit: { submit() }
                    )
                    .textFieldStyle(.plain)
                    .font(.system(size: 13, weight: .medium).monospacedDigit())
                    .foregroundColor(palette.ink)
                    .padding(.horizontal, 9)
                    .frame(height: 30)
                    .background(RoundedRectangle(cornerRadius: 9, style: .continuous).fill(palette.surfaceSubtle))
                    .overlay(RoundedRectangle(cornerRadius: 9, style: .continuous).strokeBorder(palette.lineStrong, lineWidth: 1))
                }

                if let err = store.model.limitError, !err.isEmpty {
                    Text(err)
                        .font(.system(size: 12))
                        .foregroundColor(palette.error)
                        .fixedSize(horizontal: false, vertical: true)
                }

                HStack(spacing: 8) {
                    Spacer(minLength: 0)
                    Button(action: {
                        store.limitPopover = nil
                        store.dispatch(["op": "clearLimitError"])
                    }) {
                        Text("取消")
                            .font(.system(size: 12, weight: .medium))
                            .foregroundColor(palette.ink)
                            .padding(.horizontal, 12)
                            .frame(height: 28)
                            .background(Capsule().fill(palette.surfaceSubtle))
                            .overlay(Capsule().strokeBorder(palette.line, lineWidth: 1))
                    }
                    .buttonStyle(.plain)

                    Button(action: { submit() }) {
                        Text(store.model.busy ? "保存中…" : "保存")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundColor(palette.onPrimary)
                            .padding(.horizontal, 14)
                            .frame(height: 28)
                            .background(Capsule().fill(palette.accent))
                    }
                    .buttonStyle(.plain)
                    .disabled(store.model.busy)
                }
            }
            .padding(14)
            .frame(width: 256)
            .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(palette.surface))
            .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).strokeBorder(palette.lineStrong, lineWidth: 1))
            .shadow(color: Color.black.opacity(0.28), radius: 16, x: 0, y: 8)
        }
    }

    private func submit() {
        guard let current = store.limitPopover else { return }
        var payload: [String: Any] = [
            "op": "saveProviderLimit",
            "id": current.providerId,
            "kind": current.kind,
            "value": current.value
        ]
        if let rev = current.expectedRevision {
            payload["expectedRevision"] = rev
        }
        store.dispatch(payload)
    }
}

struct QuickQuotaDetailOverlayView: View {
    @ObservedObject var store: QuickPanelStore
    let provider: ProviderCardModel
    let palette: QuickPalette

    private var detail: QuotaDetailModel { provider.quota.detail }

    var body: some View {
        ZStack {
            Color.black.opacity(palette.isDark ? 0.38 : 0.18)
                .contentShape(Rectangle())
                .onTapGesture { store.quotaDetailProviderId = nil }

            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 8) {
                    Text("\(provider.name) 额度")
                        .font(.system(size: 13.5, weight: .semibold))
                        .foregroundColor(palette.ink)
                        .lineLimit(1)
                    Spacer(minLength: 4)
                    Button(action: { store.quotaDetailProviderId = nil }) {
                        Image(systemName: "xmark")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundColor(palette.muted)
                            .frame(width: 24, height: 24)
                    }
                    .buttonStyle(.plain)
                }

                ScrollView(.vertical, showsIndicators: true) {
                    VStack(alignment: .leading, spacing: 9) {
                        if let banner = detail.keyOnlyBanner {
                            Text(banner)
                                .font(.system(size: 11.5))
                                .foregroundColor(palette.muted)
                        }

                        HStack(spacing: 8) {
                            Text(detail.headline)
                                .font(.system(size: 14, weight: .semibold).monospacedDigit())
                                .foregroundColor(palette.quotaColor(provider.quota.tone))
                            Spacer(minLength: 4)
                            if let src = detail.sourceLabel {
                                Text(src)
                                    .font(.system(size: 11.5))
                                    .foregroundColor(palette.muted)
                            }
                            if let updated = detail.updatedTime {
                                Text(updated)
                                    .font(.system(size: 11.5).monospacedDigit())
                                    .foregroundColor(palette.muted)
                                    .help(detail.updatedTitle ?? "")
                            }
                            Button(action: {
                                store.dispatch(["op": "refreshQuota", "id": provider.id])
                            }) {
                                Image(systemName: "arrow.clockwise")
                                    .font(.system(size: 11.5, weight: .medium))
                                    .foregroundColor(palette.muted)
                                    .frame(width: 22, height: 22)
                            }
                            .buttonStyle(.plain)
                            .disabled(provider.quota.refreshDisabled)
                        }

                        if let warn = detail.keyWarning {
                            Text(warn)
                                .font(.system(size: 11.5))
                                .foregroundColor(palette.warning)
                        }

                        if let err = detail.errorText {
                            Text(err)
                                .font(.system(size: 11.5))
                                .foregroundColor(palette.error)
                                .fixedSize(horizontal: false, vertical: true)
                        }

                        ForEach(detail.subscriptions) { sub in
                            VStack(alignment: .leading, spacing: 5) {
                                HStack(spacing: 6) {
                                    Text(sub.title)
                                        .font(.system(size: 12, weight: .semibold))
                                        .foregroundColor(palette.ink)
                                    if let badge = sub.statusBadge {
                                        Text(badge)
                                            .font(.system(size: 11))
                                            .foregroundColor(palette.warning)
                                    }
                                    Spacer(minLength: 4)
                                    if let exp = sub.expiresText {
                                        Text(exp)
                                            .font(.system(size: 11).monospacedDigit())
                                            .foregroundColor(palette.muted)
                                    }
                                }
                                planTable(label: "周期", unit: sub.unitBadge, rows: sub.plans)
                            }
                            .padding(8)
                            .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(palette.surfaceSubtle))
                        }

                        if !detail.standalonePlans.isEmpty {
                            VStack(alignment: .leading, spacing: 5) {
                                planTable(label: "额度", unit: detail.standaloneUnit, rows: detail.standalonePlans)
                            }
                            .padding(8)
                            .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(palette.surfaceSubtle))
                        }

                        if let exp = detail.expiryText {
                            Text(exp)
                                .font(.system(size: 11.5).monospacedDigit())
                                .foregroundColor(palette.muted)
                        }

                        if !detail.usageRows.isEmpty {
                            VStack(alignment: .leading, spacing: 6) {
                                Button(action: { store.quotaUsageExpanded.toggle() }) {
                                    HStack(spacing: 5) {
                                        Image(systemName: store.quotaUsageExpanded ? "chevron.down" : "chevron.right")
                                            .font(.system(size: 10, weight: .semibold))
                                        Text("用量统计")
                                            .font(.system(size: 12, weight: .medium))
                                    }
                                    .foregroundColor(palette.muted)
                                }
                                .buttonStyle(.plain)

                                if store.quotaUsageExpanded {
                                    VStack(spacing: 4) {
                                        HStack {
                                            Text("时间").frame(width: 44, alignment: .leading)
                                            Text("请求").frame(maxWidth: .infinity, alignment: .trailing)
                                            Text("Token").frame(maxWidth: .infinity, alignment: .trailing)
                                            Text("费用").frame(maxWidth: .infinity, alignment: .trailing)
                                        }
                                        .font(.system(size: 11, weight: .medium))
                                        .foregroundColor(palette.muted)

                                        ForEach(detail.usageRows) { row in
                                            HStack {
                                                Text(row.label)
                                                    .foregroundColor(palette.muted)
                                                    .frame(width: 44, alignment: .leading)
                                                Text(row.requests)
                                                    .frame(maxWidth: .infinity, alignment: .trailing)
                                                Text(row.tokens)
                                                    .frame(maxWidth: .infinity, alignment: .trailing)
                                                Text(row.cost)
                                                    .frame(maxWidth: .infinity, alignment: .trailing)
                                            }
                                            .font(.system(size: 11.5).monospacedDigit())
                                            .foregroundColor(palette.ink)
                                        }
                                    }
                                    .padding(8)
                                    .background(RoundedRectangle(cornerRadius: 9, style: .continuous).fill(palette.surfaceSubtle))
                                }
                            }
                        }
                    }
                }
                .frame(maxHeight: 360)
            }
            .padding(14)
            .frame(width: 372)
            .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(palette.surface))
            .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).strokeBorder(palette.lineStrong, lineWidth: 1))
            .shadow(color: Color.black.opacity(0.32), radius: 18, x: 0, y: 8)
        }
    }

    private func planTable(label: String, unit: String?, rows: [QuotaPlanRowModel]) -> some View {
        VStack(spacing: 4) {
            HStack {
                Text(unit != nil ? "\(label) (\(unit!))" : label)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Text("剩余")
                    .frame(width: 82, alignment: .trailing)
                Text("已用 / 上限")
                    .frame(width: 112, alignment: .trailing)
            }
            .font(.system(size: 11, weight: .medium))
            .foregroundColor(palette.muted)

            ForEach(rows) { row in
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(row.name)
                            .font(.system(size: 11.5, weight: .medium))
                            .foregroundColor(palette.ink)
                        if let reset = row.resetText {
                            Text(reset)
                                .font(.system(size: 10.5).monospacedDigit())
                                .foregroundColor(palette.muted)
                                .help(row.resetTitle ?? reset)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)

                    Text(row.remainingText)
                        .font(.system(size: 11.5, weight: .medium).monospacedDigit())
                        .foregroundColor(palette.ink)
                        .frame(width: 82, alignment: .trailing)

                    Text("\(row.usedText) / \(row.totalText)")
                        .font(.system(size: 11.5).monospacedDigit())
                        .foregroundColor(palette.muted)
                        .frame(width: 112, alignment: .trailing)
                }
            }
        }
    }
}

// MARK: - Root SwiftUI View

struct QuickPanelRootView: View {
    @ObservedObject var store: QuickPanelStore
    @Environment(\.colorScheme) private var systemColorScheme

    private var palette: QuickPalette {
        let isDark: Bool
        switch store.model.appearanceMode {
        case "dark":
            isDark = true
        case "light":
            isDark = false
        default:
            isDark = (systemColorScheme == .dark)
        }
        return QuickPalette(isDark: isDark)
    }

    var body: some View {
        ZStack {
            VisualEffectSurface(isDark: palette.isDark)
            palette.canvas.opacity(palette.isDark ? 0.88 : 0.90)

            VStack(spacing: 8) {
                QuickFreezeCardView(
                    freeze: store.model.freeze,
                    palette: palette,
                    onToggle: { store.dispatch(["op": "toggleFreeze"]) }
                )

                QuickToolbarView(
                    clients: store.model.clients,
                    pinned: store.model.pinned,
                    busy: store.model.busy,
                    palette: palette,
                    onSelectClient: { cid in store.dispatch(["op": "selectClient", "clientId": cid]) },
                    onTogglePin: { store.dispatch(["op": "togglePinned"]) },
                    onClose: { store.dispatch(["op": "closePanel"]) }
                )

                ScrollView(.vertical, showsIndicators: true) {
                    VStack(alignment: .leading, spacing: 8) {
                        if store.decodeFailed {
                            Text("面板数据格式不符，显示的是上一次的内容")
                                .font(.system(size: 12))
                                .foregroundColor(palette.error)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, 12)
                                .padding(.vertical, 9)
                                .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(palette.errorSoft))
                                .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(palette.error.opacity(0.32), lineWidth: 1))
                        }

                        if let err = store.model.errorBanner, !err.isEmpty {
                            Text(err)
                                .font(.system(size: 12))
                                .foregroundColor(palette.error)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, 12)
                                .padding(.vertical, 9)
                                .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(palette.errorSoft))
                                .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(palette.error.opacity(0.32), lineWidth: 1))
                        }

                        if let notice = store.model.noticeBanner, !notice.isEmpty {
                            Text(notice)
                                .font(.system(size: 12))
                                .foregroundColor(palette.ink)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, 12)
                                .padding(.vertical, 9)
                                .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(palette.accentSoft))
                                .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(palette.accent.opacity(0.28), lineWidth: 1))
                        }

                        if !store.model.clientVisible {
                            emptyCard(
                                title: "未显示平台",
                                buttonTitle: "设置平台",
                                action: { store.dispatch(["op": "openMain", "page": "settings"]) }
                            )
                        } else if let gateway = store.model.gateway {
                            QuickGatewayBarView(
                                gateway: gateway,
                                busy: store.model.busy,
                                palette: palette,
                                onToggleMode: { store.dispatch(["op": "toggleGatewayMode"]) },
                                onTogglePower: { store.dispatch(["op": "toggleGatewayPower"]) }
                            )

                            if let badge = gateway.waitingBadge {
                                Text(badge)
                                    .font(.system(size: 12, weight: .semibold).monospacedDigit())
                                    .foregroundColor(palette.warning)
                                    .padding(.horizontal, 12)
                                    .padding(.vertical, 5)
                                    .background(Capsule().fill(palette.warningSoft))
                                    .overlay(Capsule().strokeBorder(palette.warning.opacity(0.32), lineWidth: 1))
                            }

                            if !gateway.longWaits.isEmpty {
                                QuickLongWaitsView(
                                    waits: gateway.longWaits,
                                    palette: palette,
                                    onCancel: { reqId in store.dispatch(["op": "cancelWait", "requestId": reqId]) }
                                )
                            }

                            if gateway.providers.isEmpty {
                                emptyCard(
                                    title: "尚无供应商",
                                    buttonTitle: "添加供应商",
                                    action: { store.dispatch(["op": "openMain", "page": "gateway"]) }
                                )
                            } else {
                                providerList(gateway: gateway)
                            }
                        } else {
                            Text("正在读取供应商…")
                                .font(.system(size: 12.5))
                                .foregroundColor(palette.muted)
                                .frame(maxWidth: .infinity)
                                .padding(.vertical, 28)
                                .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(palette.surface))
                        }
                    }
                    .padding(.horizontal, 1)
                }

                QuickControlsDockView(
                    controls: store.model.controls,
                    palette: palette,
                    dispatch: { store.dispatch($0) }
                )

                HStack {
                    Button(action: { store.dispatch(["op": "openMain"]) }) {
                        HStack(spacing: 4) {
                            Text("打开 Vela")
                            Image(systemName: "arrow.up.right")
                                .font(.system(size: 10, weight: .semibold))
                        }
                        .font(.system(size: 11.5, weight: .medium))
                        .foregroundColor(palette.muted)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 4)
                    }
                    .buttonStyle(.plain)

                    Spacer()

                    Button(action: { store.dispatch(["op": "openMain", "page": "gateway"]) }) {
                        Text("管理供应商")
                            .font(.system(size: 11.5, weight: .medium))
                            .foregroundColor(palette.muted)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 4)
                    }
                    .buttonStyle(.plain)
                }
                .frame(minHeight: 28)
            }
            .padding(10)

            if let limitDraft = store.limitPopover {
                QuickLimitOverlayView(store: store, draft: limitDraft, palette: palette)
            } else if let qid = store.quotaDetailProviderId,
                      let provider = store.model.gateway?.providers.first(where: { $0.id == qid }) {
                QuickQuotaDetailOverlayView(store: store, provider: provider, palette: palette)
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 20, style: .continuous)
                .strokeBorder(palette.lineStrong, lineWidth: 1)
        )
        .environment(\.colorScheme, palette.isDark ? .dark : .light)
    }

    private func emptyCard(title: String, buttonTitle: String, action: @escaping () -> Void) -> some View {
        VStack(spacing: 10) {
            Text(title)
                .font(.system(size: 13))
                .foregroundColor(palette.muted)
            Button(action: action) {
                Text(buttonTitle)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundColor(palette.accent)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 5)
                    .background(Capsule().fill(palette.accentSoft))
                    .overlay(Capsule().strokeBorder(palette.accent.opacity(0.32), lineWidth: 1))
            }
            .buttonStyle(.plain)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 28)
        .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(palette.surface))
        .overlay(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .strokeBorder(style: StrokeStyle(lineWidth: 1, dash: [5, 4]))
                .foregroundColor(palette.lineStrong)
        )
    }

    private func providerList(gateway: GatewayCardModel) -> some View {
        let orderedProviders: [ProviderCardModel] = {
            guard let drag = store.activeDrag else { return gateway.providers }
            let map = Dictionary(uniqueKeysWithValues: gateway.providers.map { ($0.id, $0) })
            return drag.currentIds.compactMap { map[$0] }
        }()
        let rowSpacing: CGFloat = 7.0
        // Rows differ in height, so the target comes from each measured row.
        let stride: (String) -> CGFloat = { id in (store.rowHeights[id] ?? 71.0) + rowSpacing }

        return VStack(spacing: rowSpacing) {
            ForEach(orderedProviders) { provider in
                QuickProviderRowView(
                    store: store,
                    provider: provider,
                    gatewayRevision: gateway.revision,
                    clientId: gateway.clientId,
                    rowDisabled: store.model.busy || (store.activeDrag != nil && store.activeDrag?.id != provider.id),
                    isDragging: store.activeDrag?.id == provider.id,
                    palette: palette,
                    onDragChanged: { translationY in
                        let ids = gateway.providers.map { $0.id }
                        guard let startIdx = ids.firstIndex(of: provider.id) else { return }
                        if store.activeDrag == nil {
                            store.activeDrag = DragSession(
                                id: provider.id,
                                initialIds: ids,
                                expectedRevision: gateway.revision,
                                currentIds: ids,
                                offsetY: translationY
                            )
                        }
                        var targetIdx = startIdx
                        var passed: CGFloat = 0
                        let neighbours = translationY >= 0
                            ? Array(ids[(startIdx + 1)...])
                            : Array(ids[..<startIdx].reversed())
                        for id in neighbours {
                            let step = stride(id)
                            guard abs(translationY) > passed + step / 2 else { break }
                            passed += step
                            targetIdx += translationY >= 0 ? 1 : -1
                        }
                        var reordered = ids
                        let item = reordered.remove(at: startIdx)
                        reordered.insert(item, at: targetIdx)
                        store.activeDrag?.currentIds = reordered
                        store.activeDrag?.offsetY = translationY
                    },
                    onDragEnded: {
                        guard let drag = store.activeDrag else { return }
                        store.activeDrag = nil
                        if drag.currentIds != drag.initialIds {
                            store.dispatch([
                                "op": "reorderProviders",
                                "ids": drag.currentIds,
                                "expectedRevision": drag.expectedRevision
                            ])
                        }
                    },
                    onMoveByStep: { step in
                        var ids = gateway.providers.map { $0.id }
                        guard let idx = ids.firstIndex(of: provider.id) else { return }
                        let next = max(0, min(ids.count - 1, idx + step))
                        guard next != idx else { return }
                        let item = ids.remove(at: idx)
                        ids.insert(item, at: next)
                        store.dispatch([
                            "op": "reorderProviders",
                            "ids": ids,
                            "expectedRevision": gateway.revision
                        ])
                    }
                )
                .background(
                    GeometryReader { geo in
                        Color.clear.preference(
                            key: ProviderRowHeightKey.self,
                            value: [provider.id: geo.size.height]
                        )
                    }
                )
            }
        }
        .coordinateSpace(name: "providerList")
        .onPreferenceChange(ProviderRowHeightKey.self) { store.rowHeights = $0 }
    }
}

// MARK: - AppKit NSPanel & Controller

final class VelaQuickPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}

final class QuickPanelController: NSObject {
    static let shared = QuickPanelController()

    private let store = QuickPanelStore()
    private var panel: VelaQuickPanel?
    private weak var statusItem: NSStatusItem?
    private var lastAnchorScreenRect: NSRect?
    private var callback: (@convention(c) (UnsafePointer<CChar>?) -> Void)?
    private var globalMouseMonitor: Any?
    private var localKeyMonitor: Any?
    private var resignObserver: NSObjectProtocol?

    private override init() {
        super.init()
        store.sendAction = { [weak self] payload in
            self?.emitToRust(payload)
        }
    }

    func setCallback(_ cb: @escaping @convention(c) (UnsafePointer<CChar>?) -> Void) {
        callback = cb
    }

    func attachStatusItem(_ raw: UnsafeRawPointer?) {
        guard let raw = raw else { return }
        statusItem = Unmanaged<NSStatusItem>.fromOpaque(raw).takeUnretainedValue()
    }

    var isVisible: Bool {
        panel?.isVisible ?? false
    }

    func updateJson(_ jsonPtr: UnsafePointer<CChar>?) {
        guard let jsonPtr = jsonPtr else { return }
        let data = Data(bytes: jsonPtr, count: strlen(jsonPtr))
        if let decoded = try? JSONDecoder().decode(QuickPanelViewModel.self, from: data) {
            store.decodeFailed = false
            store.apply(decoded)
            applyPanelAppearance(decoded.appearanceMode)
        } else {
            // Keep the last good view and say why it stopped updating.
            store.decodeFailed = true
        }
    }

    func toggle(jsonPtr: UnsafePointer<CChar>?) -> Bool {
        if let panel = panel, panel.isVisible {
            hide(notifyRust: true)
            return false
        }
        updateJson(jsonPtr)
        show()
        return true
    }

    func show() {
        let p = ensurePanel()
        applyPanelAppearance(store.model.appearanceMode)
        positionPanel(p)
        installMonitors()
        p.orderFrontRegardless()
        p.makeKey()
    }

    func hide(notifyRust: Bool) {
        removeMonitors()
        store.resetTransient()
        panel?.orderOut(nil)
        if notifyRust {
            emitToRust(["op": "panelVisibility", "visible": false])
        }
    }

    private func ensurePanel() -> VelaQuickPanel {
        if let existing = panel {
            return existing
        }
        let style: NSWindow.StyleMask = [.borderless, .nonactivatingPanel, .fullSizeContentView]
        let p = VelaQuickPanel(
            contentRect: NSRect(x: 0, y: 0, width: 420, height: 720),
            styleMask: style,
            backing: .buffered,
            defer: false
        )
        p.title = "Vela 快捷面板"
        p.isFloatingPanel = true
        p.level = .popUpMenu
        p.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient]
        p.hidesOnDeactivate = false
        p.isOpaque = false
        p.backgroundColor = .clear
        p.hasShadow = true
        p.isMovable = false

        let hosting = NSHostingView(rootView: QuickPanelRootView(store: store))
        hosting.frame = NSRect(x: 0, y: 0, width: 420, height: 720)
        hosting.autoresizingMask = [.width, .height]
        p.contentView = hosting

        resignObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.didResignKeyNotification,
            object: p,
            queue: .main
        ) { [weak self] _ in
            self?.handleDidResignKey()
        }

        panel = p
        return p
    }

    private func applyPanelAppearance(_ mode: String) {
        guard let panel = panel else { return }
        switch mode {
        case "dark":
            panel.appearance = NSAppearance(named: .darkAqua)
        case "light":
            panel.appearance = NSAppearance(named: .aqua)
        default:
            panel.appearance = nil
        }
    }

    private func currentTrayScreenRect() -> NSRect? {
        if let button = statusItem?.button, let win = button.window {
            let inWin = button.convert(button.bounds, to: nil)
            let rect = win.convertToScreen(inWin)
            lastAnchorScreenRect = rect
            return rect
        }
        return lastAnchorScreenRect
    }

    private func isMouseInsideTrayButton() -> Bool {
        guard let rect = currentTrayScreenRect() else { return false }
        return rect.insetBy(dx: -3, dy: -3).contains(NSEvent.mouseLocation)
    }

    private func positionPanel(_ p: NSPanel) {
        let trayRect = currentTrayScreenRect()
        let mouse = NSEvent.mouseLocation
        let anchorPoint = trayRect.map { CGPoint(x: $0.midX, y: $0.minY) } ?? mouse

        let screen = statusItem?.button?.window?.screen
            ?? NSScreen.screens.first(where: { $0.frame.contains(anchorPoint) })
            ?? NSScreen.main
            ?? NSScreen.screens.first

        let visible = screen?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
        let width = min(420.0, visible.width)
        let height = min(720.0, visible.height)

        let rawX = anchorPoint.x - width / 2.0
        let x = min(max(rawX, visible.minX), visible.maxX - width)

        let rawY: CGFloat
        if let tray = trayRect {
            if tray.minY >= visible.midY {
                rawY = tray.minY - height - 4.0
            } else {
                rawY = tray.maxY + 4.0
            }
        } else {
            rawY = visible.maxY - height - 4.0
        }
        let y = min(max(rawY, visible.minY), visible.maxY - height)
        p.setFrame(NSRect(x: x, y: y, width: width, height: height), display: true)
    }

    private func handleDidResignKey() {
        guard let p = panel, p.isVisible, !store.model.pinned else { return }
        if NSApp.keyWindow?.parent === p { return }
        if isMouseInsideTrayButton() { return }
        hide(notifyRust: true)
    }

    private func installMonitors() {
        removeMonitors()
        globalMouseMonitor = NSEvent.addGlobalMonitorForEvents(
            matching: [.leftMouseDown, .rightMouseDown, .otherMouseDown]
        ) { [weak self] _ in
            guard let self = self,
                  let p = self.panel,
                  p.isVisible,
                  !self.store.model.pinned else { return }
            let mouse = NSEvent.mouseLocation
            if p.frame.contains(mouse) || self.isMouseInsideTrayButton() {
                return
            }
            self.hide(notifyRust: true)
        }

        localKeyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self = self,
                  let p = self.panel,
                  p.isVisible,
                  p.isKeyWindow else { return event }
            if event.keyCode == 53 { // Escape
                if let textView = p.firstResponder as? NSTextView, textView.hasMarkedText() {
                    return event
                }
                if !self.store.handleEscape() {
                    self.hide(notifyRust: true)
                }
                return nil
            }
            return event
        }
    }

    private func removeMonitors() {
        if let monitor = globalMouseMonitor {
            NSEvent.removeMonitor(monitor)
            globalMouseMonitor = nil
        }
        if let monitor = localKeyMonitor {
            NSEvent.removeMonitor(monitor)
            localKeyMonitor = nil
        }
    }

    private func emitToRust(_ payload: [String: Any]) {
        guard let cb = callback,
              let data = try? JSONSerialization.data(withJSONObject: payload, options: []),
              let json = String(data: data, encoding: .utf8) else { return }
        json.withCString { ptr in
            cb(ptr)
        }
    }
}

// MARK: - C FFI Entrypoints

@_cdecl("vela_macos_effective_dark")
public func vela_macos_effective_dark() -> Bool {
    if Thread.isMainThread, let app = NSApp {
        return app.effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
    }
    return UserDefaults.standard.string(forKey: "AppleInterfaceStyle") == "Dark"
}

@_cdecl("vela_quick_init")
public func vela_quick_init(_ callback: @escaping @convention(c) (UnsafePointer<CChar>?) -> Void) {
    QuickPanelController.shared.setCallback(callback)
}

@_cdecl("vela_quick_attach_status_item")
public func vela_quick_attach_status_item(_ raw: UnsafeRawPointer?) {
    QuickPanelController.shared.attachStatusItem(raw)
}

@_cdecl("vela_quick_toggle")
public func vela_quick_toggle(_ jsonPtr: UnsafePointer<CChar>?) -> Bool {
    QuickPanelController.shared.toggle(jsonPtr: jsonPtr)
}

@_cdecl("vela_quick_render")
public func vela_quick_render(_ jsonPtr: UnsafePointer<CChar>?) {
    QuickPanelController.shared.updateJson(jsonPtr)
}

@_cdecl("vela_quick_hide")
public func vela_quick_hide() {
    QuickPanelController.shared.hide(notifyRust: false)
}

@_cdecl("vela_quick_is_visible")
public func vela_quick_is_visible() -> Bool {
    QuickPanelController.shared.isVisible
}
