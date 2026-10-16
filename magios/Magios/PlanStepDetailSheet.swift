import SwiftUI

/// One step, in a half-height sheet. Read-only by default; in edit mode every
/// field is editable — task, confidence, tool, parameters, expected outputs,
/// metadata, start step, and dependencies through a cycle-safe picker (web
/// `PlanGraphView` details panel). Edits go straight into the draft.
struct PlanStepDetailSheet: View {
    @ObservedObject var model: PlanInspectorModel
    let stepId: String

    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.dismiss) private var dismiss
    @State private var newParamKey = ""
    @State private var newParamValue = ""
    @State private var newMetaKey = ""
    @State private var newMetaValue = ""
    @State private var confirmRemove = false

    private var graph: PlanGraph? { model.shownGraph }
    private var step: PlanStep? { graph?.step(stepId) }
    private var editing: Bool { model.isEditing }

    var body: some View {
        NavigationStack {
            Group {
                if let step, let graph {
                    Form {
                        if editing { editSections(step, graph) } else { readSections(step, graph) }
                    }
                    .scrollContentBackground(.hidden)
                } else {
                    ContentUnavailableView("Step removed", systemImage: "square.dashed",
                                           description: Text("This step is no longer in the plan."))
                }
            }
            .background(theme.backgroundColor.ignoresSafeArea())
            .navigationTitle(editing ? "Edit step" : "Step")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
            .confirmationDialog("Remove this step?", isPresented: $confirmRemove, titleVisibility: .visible) {
                Button("Remove step", role: .destructive) {
                    model.editDraft { $0.removeStep(stepId) }
                    dismiss()
                }
            } message: {
                Text("Steps that depend on it lose that dependency. Nothing is saved until you tap Save.")
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }

    // MARK: Read-only

    @ViewBuilder
    private func readSections(_ step: PlanStep, _ graph: PlanGraph) -> some View {
        Section("Task") {
            Text(step.task.isEmpty ? "—" : step.task).font(.themed(15)).foregroundColor(theme.textColor)
            if let mark = model.runMarks[step.id] {
                HStack(spacing: 6) {
                    StatusDot(tone: PlanStyle.tone(for: mark))
                    Text("This run: \(PlanStyle.markLabel(mark))").font(.themed(13))
                }
            }
        }
        Section("Execution") {
            row("Confidence", PlanStyle.percent(step.confidence),
                color: PlanStyle.color(for: PlanConfidenceBand(step.confidence), theme: theme))
            if let agent = step.providingAgentId, step.isDelegated { row("Delegated to", agent) }
            row("Tool", step.hasResolvedTool ? (step.tool ?? "") : "Not resolved",
                color: step.hasResolvedTool ? nil : theme.warningColor, mono: step.hasResolvedTool)
            row("Start step", step.isStart ? "Yes" : "No")
        }
        if !step.dependsOn.isEmpty {
            Section("Depends on") {
                ForEach(step.dependsOn, id: \.self) { dep in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(title(of: dep, in: graph)).font(.themed(14))
                        if let reason = graph.reason(from: dep, to: step.id), !reason.isEmpty {
                            Text(reason).font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                        }
                    }
                }
            }
        }
        if !step.parameters.isEmpty {
            Section("Parameters") {
                ForEach(step.parameters) { row($0.key, $0.value.displayText, mono: true) }
            }
        }
        if !step.expectedOutputs.isEmpty {
            Section("Expected outputs") {
                ForEach(Array(step.expectedOutputs.enumerated()), id: \.offset) { Text($0.element).font(.themed(14)) }
            }
        }
        if !step.metadata.isEmpty {
            Section("Metadata") { ForEach(step.metadata) { row($0.key, $0.value) } }
        }
        Section("Identity") {
            row("ID", step.id, mono: true)
            if let hash = step.stepHash { row("Step hash", hash, mono: true) }
        }
    }

    private func row(_ label: String, _ value: String, color: Color? = nil, mono: Bool = false) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(label).font(.themed(13)).foregroundColor(theme.secondaryTextColor)
            Spacer(minLength: 12)
            Text(value)
                .font(mono ? .themedMono(13) : .themed(14))
                .foregroundColor(color ?? theme.textColor)
                .multilineTextAlignment(.trailing)
                .textSelection(.enabled)
        }
        .accessibilityElement(children: .combine)
    }

    private func title(of id: String, in graph: PlanGraph) -> String {
        graph.step(id).map { $0.task.isEmpty ? $0.id : $0.task } ?? id
    }

    // MARK: Editing

    private func binding<Value>(_ keyPath: WritableKeyPath<PlanStep, Value>, default fallback: Value) -> Binding<Value> {
        Binding(
            get: { model.draft?.step(stepId)?[keyPath: keyPath] ?? fallback },
            set: { value in model.editDraft { $0.updateStep(stepId) { $0[keyPath: keyPath] = value } } })
    }

    @ViewBuilder
    private func editSections(_ step: PlanStep, _ graph: PlanGraph) -> some View {
        Section("Task") {
            TextField("What this step does", text: binding(\.task, default: ""), axis: .vertical)
                .lineLimit(2...6)
                .accessibilityIdentifier("plan-step-task-field")
        }
        Section("Confidence · \(PlanStyle.percent(step.confidence))") {
            let confidence = binding(\.confidence, default: 0.5)
            Slider(value: Binding(get: { confidence.wrappedValue },
                                  set: { confidence.wrappedValue = (($0 * 20).rounded() / 20) }),
                   in: 0...1, step: 0.05)
                .accessibilityLabel("Confidence")
            Stepper("Adjust by 5%", value: Binding(get: { confidence.wrappedValue },
                                                     set: { confidence.wrappedValue = min(max((($0 * 20).rounded() / 20), 0), 1) }),
                    in: 0...1, step: 0.05)
                .font(.themed(13))
        }
        Section {
            TextField("Tool id (empty = unresolved)", text: Binding(
                get: { model.draft?.step(stepId)?.tool ?? "" },
                set: { value in model.editDraft { $0.updateStep(stepId) { $0.tool = value.isEmpty ? nil : value } } }))
                .font(.themedMono(14))
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
        } header: { Text("Tool") } footer: {
            Text("Type the tool id. Leave it empty to let the planner resolve one.").font(.themed(12))
        }
        dependencySection(step, graph)
        parameterSection(step)
        outputSection
        metadataSection(step)
        Section {
            Button(role: .destructive) { confirmRemove = true } label: {
                Label("Remove step", systemImage: "trash").frame(minHeight: Lively.tapMin)
            }
        }
    }

    @ViewBuilder
    private func dependencySection(_ step: PlanStep, _ graph: PlanGraph) -> some View {
        let candidates = graph.dependencyCandidates(for: stepId)
        Section {
            Toggle("Start step (no dependencies)", isOn: Binding(
                get: { model.draft?.step(stepId)?.isStart ?? true },
                set: { on in if on { model.makeStart(stepId) } }))
                .disabled(step.isStart)
            ForEach(step.dependsOn, id: \.self) { dep in
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(title(of: dep, in: graph)).font(.themed(14))
                        if let reason = graph.reason(from: dep, to: stepId), !reason.isEmpty {
                            Text(reason).font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                        }
                    }
                    Spacer()
                    Button { model.setDependency(stepId, on: dep, false) } label: {
                        Image(systemName: "minus.circle.fill").foregroundColor(theme.dangerColor)
                            .frame(width: Lively.tapMin, height: Lively.tapMin)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Remove dependency on \(title(of: dep, in: graph))")
                }
            }
            Menu {
                ForEach(candidates) { candidate in
                    Button(candidate.task.isEmpty ? candidate.id : candidate.task) {
                        model.setDependency(stepId, on: candidate.id, true)
                    }
                }
            } label: {
                Label("Add dependency", systemImage: "plus.circle").frame(minHeight: Lively.tapMin)
            }
            .disabled(candidates.isEmpty)
            .accessibilityIdentifier("plan-step-add-dependency")
        } header: { Text("Depends on") } footer: {
            Text(candidates.isEmpty && !graph.steps.isEmpty
                 ? "No other step can be added without making a cycle."
                 : "Only steps that cannot create a cycle are offered.")
                .font(.themed(12))
        }
    }

    @ViewBuilder
    private func parameterSection(_ step: PlanStep) -> some View {
        Section("Parameters") {
            ForEach(step.parameters) { parameter in
                HStack(spacing: 8) {
                    Text(parameter.key).font(.themedMono(13)).foregroundColor(theme.secondaryTextColor)
                    TextField("value", text: Binding(
                        get: { model.draft?.step(stepId)?.parameters.first { $0.key == parameter.key }?.value.displayText ?? "" },
                        set: { text in
                            let original = model.draft?.step(stepId)?.raw["parameters"]?[parameter.key]
                            model.editDraft { $0.updateStep(stepId) { step in
                                if let index = step.parameters.firstIndex(where: { $0.key == parameter.key }) {
                                    step.parameters[index].value = PlanParameter.value(fromText: text, previous: original)
                                }
                            } }
                        }))
                        .font(.themedMono(13))
                        .textInputAutocapitalization(.never)
                }
            }
            .onDelete { offsets in
                model.editDraft { $0.updateStep(stepId) { $0.parameters.remove(atOffsets: offsets) } }
            }
            keyValueAdder(key: $newParamKey, value: $newParamValue, placeholder: "parameter") { key, value in
                model.editDraft { $0.updateStep(stepId) { step in
                    step.parameters.removeAll { $0.key == key }
                    step.parameters.append(PlanParameter(key: key, value: .string(value)))
                } }
            }
        }
    }

    private var outputSection: some View {
        let outputs = model.draft?.step(stepId)?.expectedOutputs ?? []
        return Section("Expected outputs") {
            ForEach(Array(outputs.indices), id: \.self) { index in
                TextField("Output", text: Binding(
                    get: { model.draft?.step(stepId)?.expectedOutputs[safe: index] ?? "" },
                    set: { text in model.editDraft { $0.updateStep(stepId) { step in
                        if step.expectedOutputs.indices.contains(index) { step.expectedOutputs[index] = text }
                    } } }))
                    .font(.themed(14))
            }
            .onDelete { offsets in
                model.editDraft { $0.updateStep(stepId) { $0.expectedOutputs.remove(atOffsets: offsets) } }
            }
            Button {
                model.editDraft { $0.updateStep(stepId) { $0.expectedOutputs.append("") } }
            } label: { Label("Add output", systemImage: "plus").frame(minHeight: Lively.tapMin) }
        }
    }

    @ViewBuilder
    private func metadataSection(_ step: PlanStep) -> some View {
        Section("Metadata") {
            ForEach(step.metadata) { entry in
                HStack(spacing: 8) {
                    Text(entry.key).font(.themedMono(13)).foregroundColor(theme.secondaryTextColor)
                    TextField("value", text: Binding(
                        get: { model.draft?.step(stepId)?.metadata.first { $0.key == entry.key }?.value ?? "" },
                        set: { text in model.editDraft { $0.updateStep(stepId) { step in
                            if let index = step.metadata.firstIndex(where: { $0.key == entry.key }) { step.metadata[index].value = text }
                        } } }))
                        .font(.themed(13))
                }
            }
            .onDelete { offsets in
                model.editDraft { $0.updateStep(stepId) { $0.metadata.remove(atOffsets: offsets) } }
            }
            keyValueAdder(key: $newMetaKey, value: $newMetaValue, placeholder: "key") { key, value in
                model.editDraft { $0.updateStep(stepId) { step in
                    step.metadata.removeAll { $0.key == key }
                    step.metadata.append(PlanKeyValue(key: key, value: value))
                } }
            }
        }
    }

    private func keyValueAdder(key: Binding<String>, value: Binding<String>, placeholder: String,
                               add: @escaping (String, String) -> Void) -> some View {
        HStack(spacing: 8) {
            TextField(placeholder, text: key)
                .font(.themedMono(13)).textInputAutocapitalization(.never).autocorrectionDisabled()
            TextField("value", text: value).font(.themed(13))
            Button {
                let trimmed = key.wrappedValue.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !trimmed.isEmpty else { return }
                add(trimmed, value.wrappedValue)
                key.wrappedValue = ""
                value.wrappedValue = ""
            } label: {
                Image(systemName: "plus.circle.fill").foregroundColor(theme.accentColor)
                    .frame(width: Lively.tapMin, height: Lively.tapMin)
            }
            .buttonStyle(.plain)
            .disabled(key.wrappedValue.trimmingCharacters(in: .whitespaces).isEmpty)
            .accessibilityLabel("Add \(placeholder)")
        }
    }
}

private extension Array {
    subscript(safe index: Int) -> Element? { indices.contains(index) ? self[index] : nil }
}
