const steps = [
  ['Tencent Enterprise Email', 'Connect the mailbox used by staff'],
  ['Google Workspace', 'Choose the Drive root containing student folders'],
  ['Automation', 'Read new mail, identify the student, upload attachments'],
];

export default function App() {
  return (
    <main className="shell">
      <section className="hero">
        <p className="eyebrow">Email Triage</p>
        <h1>Route student email attachments automatically.</h1>
        <p className="lede">
          A local-first desktop app for Tencent Enterprise Email and Google Drive.
        </p>
      </section>

      <section className="panel" aria-label="Setup status">
        <div className="panelHeader">
          <div>
            <p className="eyebrow">MVP</p>
            <h2>Connection setup</h2>
          </div>
          <span className="status">Not configured</span>
        </div>

        <ol className="steps">
          {steps.map(([title, description], index) => (
            <li key={title}>
              <span className="stepNumber">{index + 1}</span>
              <div>
                <strong>{title}</strong>
                <p>{description}</p>
              </div>
              <button type="button" disabled>
                Coming next
              </button>
            </li>
          ))}
        </ol>
      </section>
    </main>
  );
}
