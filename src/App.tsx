import { useEffect } from "react";
import { Group, Panel, Separator } from "react-resizable-panels";
import { useAppStore } from "./store/useAppStore";
import { HeaderBar } from "./components/HeaderBar";
import { FileList } from "./components/FileList";
import { CommitPanel } from "./components/CommitPanel";
import { DiffViewer } from "./components/DiffViewer";
import { Toasts } from "./components/Toasts";
import "./App.css";

function NoRepoState() {
  const openRepoViaPicker = useAppStore((s) => s.openRepoViaPicker);
  const loading = useAppStore((s) => s.loading);

  return (
    <div className="no-repo">
      <div className="no-repo__card">
        <h1 className="no-repo__title">gitrx</h1>
        <p className="no-repo__subtitle">A modern take on the GitX staging screen.</p>
        <button
          type="button"
          className="no-repo__open"
          onClick={() => void openRepoViaPicker()}
          disabled={loading}
        >
          {loading ? "Opening…" : "Open Repository…"}
        </button>
      </div>
    </div>
  );
}

function RepoView() {
  const status = useAppStore((s) => s.status);

  if (!status) return null;

  return (
    <div className="repo-view">
      <HeaderBar status={status} />
      <Group orientation="vertical" className="repo-view__body">
        <Panel defaultSize={65} minSize={20} className="repo-view__diff">
          <DiffViewer />
        </Panel>
        <Separator className="resize-handle resize-handle--horizontal" />
        <Panel defaultSize={35} minSize={15} className="repo-view__bottom">
          <Group orientation="horizontal" className="staging">
            <Panel defaultSize={33} minSize={15} className="staging__pane">
              <FileList title="Unstaged Changes" files={status.unstaged} staged={false} />
            </Panel>
            <Separator className="resize-handle resize-handle--vertical" />
            <Panel defaultSize={34} minSize={18} className="staging__pane">
              <CommitPanel />
            </Panel>
            <Separator className="resize-handle resize-handle--vertical" />
            <Panel defaultSize={33} minSize={15} className="staging__pane">
              <FileList title="Staged Changes" files={status.staged} staged={true} />
            </Panel>
          </Group>
        </Panel>
      </Group>
    </div>
  );
}

function App() {
  const status = useAppStore((s) => s.status);
  const initialize = useAppStore((s) => s.initialize);
  const initWatcher = useAppStore((s) => s.initWatcher);

  useEffect(() => {
    void initialize();
    void initWatcher();
  }, [initialize, initWatcher]);

  return (
    <div className="app">
      {status ? <RepoView /> : <NoRepoState />}
      <Toasts />
    </div>
  );
}

export default App;
