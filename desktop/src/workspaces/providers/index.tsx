import { StateNotice } from "../../shell/StateNotice";

// Providers workspace: reserved for the provider status and setup slices of EPIC B. Provider
// credentials stay in Rust; nothing here ever receives one. The missing-CLI and failed-sign-in
// notices (shell/StateNotice) are what this workspace shows once provider discovery feeds it.
export function ProvidersWorkspace() {
  return (
    <StateNotice kind="empty" title="No provider data yet">
      Provider discovery and sign-in status will list here. Nothing is configured from the desktop app yet.
    </StateNotice>
  );
}
