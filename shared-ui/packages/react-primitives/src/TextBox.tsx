import { useId, type InputHTMLAttributes } from 'react';

export interface TextBoxProps extends Omit<InputHTMLAttributes<HTMLInputElement>, 'size'> {
  label: string;
  helperText?: string;
  /** Marks the field invalid and shows the message, tied to the input with aria-describedby. */
  error?: string;
}

/** A labelled text input. The label is a real <label>, and helper or error text is announced with the field. */
export function TextBox({ label, helperText, error, id, className, ...rest }: TextBoxProps) {
  const auto = useId();
  const inputId = id ?? auto;
  const noteId = `${inputId}-note`;
  const note = error ?? helperText;
  return (
    <div className="ui-field">
      <label htmlFor={inputId} className="ui-field__label">
        {label}
      </label>
      <input
        type="text"
        {...rest}
        id={inputId}
        className={['ui-input', className].filter(Boolean).join(' ')}
        aria-invalid={error ? true : undefined}
        aria-describedby={note ? noteId : undefined}
      />
      {note ? (
        <span id={noteId} className={error ? 'ui-field__note ui-field__note--error' : 'ui-field__note'}>
          {note}
        </span>
      ) : null}
    </div>
  );
}
