import { FormEvent, useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

type MailConfig = {
  host: string;
  port: number;
  username: string;
  mailbox: string;
};

type AppConfig = {
  mail: MailConfig | null;
  googleClientId: string | null;
  googleEmail: string | null;
  driveRootId: string | null;
  pollIntervalSeconds: number;
  processedMailbox: string;
  reviewMailbox: string;
};

type DriveFile = {
  id: string;
  name: string;
  mimeType?: string | null;
  parents?: string[] | null;
};

type ProcessingResult = {
  uid: number;
  messageId?: string | null;
  subject?: string | null;
  studentName?: string | null;
  folderId?: string | null;
  uploadedFileIds: string[];
  skippedExistingFiles: string[];
  status: 'uploaded' | 'processedNoAttachments' | 'needsReview' | 'failed';
  detail: string;
};

const defaults: AppConfig = {
  mail: null,
  googleClientId: null,
  googleEmail: null,
  driveRootId: null,
  pollIntervalSeconds: 60,
  processedMailbox: 'EmailTriage-Processed',
  reviewMailbox: 'EmailTriage-NeedsReview',
};

export default function App() {
  const [config, setConfig] = useState<AppConfig>(defaults);
  const [mail, setMail] = useState<MailConfig>({
    host: 'imap.exmail.qq.com',
    port: 993,
    username: '',
    mailbox: 'INBOX',
  });
  const [password, setPassword] = useState('');
  const [googleClientId, setGoogleClientId] = useState(
    import.meta.env.VITE_GOOGLE_CLIENT_ID ?? '',
  );
  const [folders, setFolders] = useState<DriveFile[]>([]);
  const [folderPath, setFolderPath] = useState<Array<{ id: string; name: string }>>([
    { id: 'root', name: 'My Drive' },
  ]);
  const [results, setResults] = useState<ProcessingResult[]>([]);
  const [autostart, setAutostartState] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<string>('');
  const [error, setError] = useState<string>('');

  useEffect(() => {
    Promise.all([invoke<AppConfig>('get_config'), invoke<boolean>('get_autostart')])
      .then(([saved, autostartEnabled]) => {
        setConfig(saved);
        setAutostartState(autostartEnabled);
        if (saved.mail) setMail(saved.mail);
        if (saved.googleClientId) setGoogleClientId(saved.googleClientId);
      })
      .catch((err) => setError(String(err)));
  }, []);

  const ready = Boolean(config.mail && config.googleEmail && config.driveRootId);
  const summary = useMemo(() => {
    return results.reduce(
      (acc, item) => {
        acc[item.status] += 1;
        return acc;
      },
      { uploaded: 0, processedNoAttachments: 0, needsReview: 0, failed: 0 },
    );
  }, [results]);

  async function runAction<T>(label: string, fn: () => Promise<T>): Promise<T | undefined> {
    setBusy(label);
    setError('');
    setNotice('');
    try {
      return await fn();
    } catch (err) {
      setError(String(err));
      return undefined;
    } finally {
      setBusy(null);
    }
  }

  async function reloadConfig() {
    const saved = await invoke<AppConfig>('get_config');
    setConfig(saved);
    return saved;
  }

  async function saveMail(event: FormEvent) {
    event.preventDefault();
    const saved = await runAction('mail', async () => {
      await invoke('save_mail_account', { mailConfig: mail, password });
      return reloadConfig();
    });
    if (saved) {
      setPassword('');
      setNotice('Tencent mailbox connected and credentials saved securely.');
    }
  }

  async function connectGoogle() {
    const connection = await runAction('google', () =>
      invoke<{ email: string }>('connect_google_account', { clientId: googleClientId.trim() }),
    );
    if (connection) {
      await reloadConfig();
      setNotice(`Connected to Google as ${connection.email}.`);
      await loadFolders('root', 'My Drive', true);
    }
  }

  async function loadFolders(parentId: string, name: string, reset = false) {
    const listed = await runAction('folders', () =>
      invoke<DriveFile[]>('list_drive_folders', { parentId }),
    );
    if (!listed) return;
    setFolders(listed.sort((a, b) => a.name.localeCompare(b.name)));
    if (reset) {
      setFolderPath([{ id: parentId, name }]);
    } else if (folderPath.at(-1)?.id !== parentId) {
      setFolderPath((current) => [...current, { id: parentId, name }]);
    }
  }

  async function goToBreadcrumb(index: number) {
    const target = folderPath[index];
    const listed = await runAction('folders', () =>
      invoke<DriveFile[]>('list_drive_folders', { parentId: target.id }),
    );
    if (!listed) return;
    setFolders(listed.sort((a, b) => a.name.localeCompare(b.name)));
    setFolderPath((current) => current.slice(0, index + 1));
  }

  async function chooseCurrentFolder() {
    const current = folderPath.at(-1);
    if (!current) return;
    const saved = await runAction('root', async () => {
      await invoke('set_drive_root', { folderId: current.id });
      return reloadConfig();
    });
    if (saved) setNotice(`Student root set to “${current.name}”.`);
  }

  async function savePolling(seconds: number) {
    const next = { ...config, pollIntervalSeconds: seconds };
    const saved = await runAction('settings', async () => {
      await invoke('save_config', { appConfig: next });
      return reloadConfig();
    });
    if (saved) setNotice(`Background polling set to every ${seconds} seconds.`);
  }

  async function toggleAutostart(enabled: boolean) {
    const changed = await runAction('autostart', async () => {
      await invoke('set_autostart', { enabled });
      return invoke<boolean>('get_autostart');
    });
    if (changed !== undefined) {
      setAutostartState(changed);
      setNotice(changed ? 'Email Triage will start automatically after login.' : 'Autostart disabled.');
    }
  }

  async function processNow() {
    const processed = await runAction('processing', () =>
      invoke<ProcessingResult[]>('process_now'),
    );
    if (processed) {
      setResults(processed);
      setNotice(`Processed ${processed.length} new message${processed.length === 1 ? '' : 's'}.`);
    }
  }

  return (
    <main className="shell">
      <section className="hero">
        <div>
          <p className="eyebrow">Email Triage</p>
          <h1>Route student attachments from email to Google Drive.</h1>
          <p className="lede">
            Local-first automation. Ambiguous student matches are held for review instead of being
            filed into the wrong folder.
          </p>
        </div>
        <div className={`readiness ${ready ? 'ready' : ''}`}>
          <span className="dot" /> {ready ? 'Automation ready' : 'Setup required'}
        </div>
      </section>

      {(error || notice) && (
        <div className={error ? 'banner error' : 'banner success'}>{error || notice}</div>
      )}

      <section className="grid">
        <article className="panel">
          <div className="panelHeader">
            <div>
              <p className="eyebrow">Step 1</p>
              <h2>Tencent Enterprise Email</h2>
            </div>
            <span className={`status ${config.mail ? 'ok' : ''}`}>
              {config.mail ? 'Connected' : 'Not connected'}
            </span>
          </div>

          <form className="form" onSubmit={saveMail}>
            <label>
              IMAP server
              <input
                value={mail.host}
                onChange={(e) => setMail({ ...mail, host: e.target.value })}
                required
              />
            </label>
            <div className="twoCol">
              <label>
                Port
                <input
                  type="number"
                  value={mail.port}
                  onChange={(e) => setMail({ ...mail, port: Number(e.target.value) })}
                  required
                />
              </label>
              <label>
                Mailbox
                <input
                  value={mail.mailbox}
                  onChange={(e) => setMail({ ...mail, mailbox: e.target.value })}
                  required
                />
              </label>
            </div>
            <label>
              Email address
              <input
                type="email"
                value={mail.username}
                onChange={(e) => setMail({ ...mail, username: e.target.value })}
                required
                autoComplete="username"
              />
            </label>
            <label>
              Mail client password / authorization code
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                required={!config.mail}
                autoComplete="current-password"
                placeholder={config.mail ? 'Enter only to reconnect' : ''}
              />
            </label>
            <button className="primary" disabled={Boolean(busy)}>
              {busy === 'mail' ? 'Testing…' : 'Save and test connection'}
            </button>
          </form>
        </article>

        <article className="panel">
          <div className="panelHeader">
            <div>
              <p className="eyebrow">Step 2</p>
              <h2>Google Drive</h2>
            </div>
            <span className={`status ${config.googleEmail ? 'ok' : ''}`}>
              {config.googleEmail ?? 'Not connected'}
            </span>
          </div>

          <div className="form">
            {!import.meta.env.VITE_GOOGLE_CLIENT_ID && (
              <label>
                Google Desktop OAuth client ID
                <input
                  value={googleClientId}
                  onChange={(e) => setGoogleClientId(e.target.value)}
                  placeholder="…apps.googleusercontent.com"
                />
                <small>Developer builds only. Release builds can provide this at build time.</small>
              </label>
            )}
            <button
              type="button"
              className="primary"
              disabled={Boolean(busy) || !googleClientId.trim()}
              onClick={connectGoogle}
            >
              {busy === 'google' ? 'Waiting for Google…' : 'Connect Google Workspace'}
            </button>

            {config.googleEmail && (
              <div className="folderPicker">
                <div className="breadcrumbs">
                  {folderPath.map((item, index) => (
                    <button type="button" key={`${item.id}-${index}`} onClick={() => goToBreadcrumb(index)}>
                      {item.name}
                    </button>
                  ))}
                </div>
                <div className="folderActions">
                  <button type="button" onClick={() => loadFolders(folderPath.at(-1)?.id ?? 'root', folderPath.at(-1)?.name ?? 'My Drive', true)}>
                    Refresh folders
                  </button>
                  <button type="button" className="primary" onClick={chooseCurrentFolder}>
                    Use this as student root
                  </button>
                </div>
                <div className="folderList">
                  {folders.map((folder) => (
                    <button type="button" key={folder.id} onClick={() => loadFolders(folder.id, folder.name)}>
                      <span>📁</span> {folder.name}
                    </button>
                  ))}
                  {!folders.length && <p className="muted">No child folders loaded.</p>}
                </div>
                {config.driveRootId && <p className="muted">Selected root ID: {config.driveRootId}</p>}
              </div>
            )}
          </div>
        </article>
      </section>

      <section className="panel automationPanel">
        <div className="panelHeader">
          <div>
            <p className="eyebrow">Step 3</p>
            <h2>Automation</h2>
          </div>
          <button className="primary" type="button" disabled={!ready || Boolean(busy)} onClick={processNow}>
            {busy === 'processing' ? 'Processing…' : 'Run now'}
          </button>
        </div>

        <div className="automationSettings">
          <label>
            Check mail every
            <select
              value={config.pollIntervalSeconds}
              disabled={Boolean(busy)}
              onChange={(e) => savePolling(Number(e.target.value))}
            >
              <option value={60}>1 minute</option>
              <option value={300}>5 minutes</option>
              <option value={900}>15 minutes</option>
            </select>
          </label>
          <label className="toggleLabel">
            <span>Start automatically after login</span>
            <input
              type="checkbox"
              checked={autostart}
              disabled={Boolean(busy)}
              onChange={(e) => toggleAutostart(e.target.checked)}
            />
          </label>
          <p>
            Closing the window keeps Email Triage running in the system tray. Success →{' '}
            <strong>{config.processedMailbox}</strong> · Ambiguous → <strong>{config.reviewMailbox}</strong>
          </p>
        </div>

        {results.length > 0 && (
          <>
            <div className="metrics">
              <Metric label="Uploaded" value={summary.uploaded} />
              <Metric label="No attachments" value={summary.processedNoAttachments} />
              <Metric label="Needs review" value={summary.needsReview} />
              <Metric label="Failed" value={summary.failed} />
            </div>
            <div className="results">
              {results.map((result) => (
                <div className="result" key={`${result.uid}-${result.messageId ?? ''}`}>
                  <span className={`pill ${result.status}`}>{result.status}</span>
                  <div>
                    <strong>{result.studentName ?? result.subject ?? `Message UID ${result.uid}`}</strong>
                    <p>{result.detail}</p>
                    {result.uploadedFileIds.length > 0 && (
                      <small>{result.uploadedFileIds.length} attachment(s) uploaded</small>
                    )}
                  </div>
                </div>
              ))}
            </div>
          </>
        )}
      </section>
    </main>
  );
}

function Metric({ label, value }: { label: string; value: number }) {
  return (
    <div className="metric">
      <strong>{value}</strong>
      <span>{label}</span>
    </div>
  );
}
