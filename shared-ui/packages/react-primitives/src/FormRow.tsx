import { useId, type ReactNode } from 'react';

/** A label on the left and a field on the right. The field gets the generated id through the render prop. */
export function FormRow({ label, children }: { label: string; children: (id: string) => ReactNode }) {
  const id = useId();
  return (
    <div className="ui-row">
      <label htmlFor={id} className="ui-row__label">
        {label}
      </label>
      <div className="ui-row__field">{children(id)}</div>
    </div>
  );
}

/** The read-only twin of FormRow: a dt/dd pair for a `<dl>`. An empty value reads as "Not set". */
export function ReadOnlyRow({ label, value }: { label: string; value: string | null | undefined }) {
  const empty = value == null || value.trim() === '';
  return (
    <div className="ui-row">
      <dt className="ui-row__label">{label}</dt>
      <dd className="ui-row__field ui-row__value">
        {empty ? (
          <>
            <span aria-hidden="true">{'—'}</span>
            <span className="ui-sr-only">Not set</span>
          </>
        ) : (
          value
        )}
      </dd>
    </div>
  );
}
