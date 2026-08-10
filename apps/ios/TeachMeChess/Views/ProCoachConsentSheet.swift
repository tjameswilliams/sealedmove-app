import SwiftUI

/// The cloud-AI disclosure the App Store requires (guideline 5.1.2(i))
/// before any game content reaches the Pro Coach service. Presented by the
/// game screen the first time Pro Coach is applied; agreeing records
/// consent locally and re-runs the switch, declining reverts the saved
/// provider to on-device so the app doesn't nag on the next launch.
///
/// Deliberately names no vendor or model — which AI backs the service is a
/// server-side detail that changes; the specifics live in the privacy
/// policy, which can say more than a sheet should.
struct ProCoachConsentSheet: View {
    let model: GameViewModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Label {
                        Text("Pro Coach runs in the cloud")
                            .fontWeight(.semibold)
                    } icon: {
                        Image(systemName: "cloud")
                    }
                    Text("Your moves, board positions, and questions to the coach are sent to our servers and processed by third-party AI services, which may run outside your country. That's what makes the pro commentary possible.")
                        .font(.callout)
                    Text("There's no account and nothing identifies you personally — requests carry only an anonymous device ID. Details are in our privacy policy.")
                        .font(.callout)
                    Link("Privacy Policy", destination: URL(string: "https://sealedmove.com/privacy")!)
                        .font(.callout)
                } footer: {
                    Text("The free on-device coach never sends your games anywhere.")
                }

                Section {
                    Button {
                        model.acceptProCoachConsent()
                        dismiss()
                    } label: {
                        Text("Agree & Continue")
                            .frame(maxWidth: .infinity)
                            .fontWeight(.semibold)
                    }
                    Button("Not Now", role: .cancel) {
                        model.declineProCoachConsent()
                        dismiss()
                    }
                    .frame(maxWidth: .infinity)
                }
            }
            .navigationTitle("Before You Start")
            .navigationBarTitleDisplayMode(.inline)
            .interactiveDismissDisabled()
        }
    }
}

#Preview {
    ProCoachConsentSheet(model: GameViewModel())
}
