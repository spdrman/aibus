import { useEffect, useId, useRef, type InputHTMLAttributes } from 'react';

export interface CheckboxProps extends Omit<InputHTMLAttributes<HTMLInputElement>, 'type' | 'size'> {
  label: string;
  /** Mixed state. Reflected on the DOM property, since there is no HTML attribute for it. */
  indeterminate?: boolean;
}

/** A native checkbox with its label. Keyboard and screen reader behaviour is the browser's own. */
export function Checkbox({ label, indeterminate = false, id, className, ...rest }: CheckboxProps) {
  const auto = useId();
  const inputId = id ?? auto;
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = indeterminate;
  }, [indeterminate]);
  return (
    <div className="ui-check">
      <input {...rest} ref={ref} id={inputId} type="checkbox" className={['ui-check__box', className].filter(Boolean).join(' ')} />
      <label htmlFor={inputId} className="ui-check__label">
        {label}
      </label>
    </div>
  );
}
