import { useState } from 'react';
import { Button, Checkbox, Dialog, DialogActions, FormRow, Tabs, TextBox } from '@sharedui/react-primitives';

/** Every shared primitive in one place, for a visual check in either theme. */
export function Showcase() {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState('');
  return (
    <main style={{ padding: 24, display: 'grid', gap: 24, maxWidth: 720 }}>
      <h1 style={{ margin: 0 }}>shared-ui</h1>
      <Tabs
        label="Showcase sections"
        items={[
          {
            id: 'controls',
            label: 'Controls',
            content: (
              <div style={{ display: 'grid', gap: 16 }}>
                <div style={{ display: 'flex', gap: 8 }}>
                  <Button>Primary</Button>
                  <Button variant="quiet">Quiet</Button>
                  <Button variant="danger">Danger</Button>
                  <Button disabled>Disabled</Button>
                </div>
                <TextBox label="Workflow name" value={name} onChange={(e) => setName(e.target.value)} helperText="Shown in the run list" />
                <TextBox label="Model" error="Pick a model" />
                <Checkbox label="Record a trace" />
                <FormRow label="Provider">{(id) => <input id={id} className="ui-input" defaultValue="claude" />}</FormRow>
              </div>
            ),
          },
          {
            id: 'dialog',
            label: 'Dialog',
            content: <Button onClick={() => setOpen(true)}>Open dialog</Button>,
          },
        ]}
      />
      <Dialog
        open={open}
        onClose={() => setOpen(false)}
        title="Delete workflow"
        footer={
          <DialogActions>
            <Button variant="quiet" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button variant="danger" onClick={() => setOpen(false)}>
              Delete
            </Button>
          </DialogActions>
        }
      >
        This removes the workflow and its recorded runs.
      </Dialog>
    </main>
  );
}
