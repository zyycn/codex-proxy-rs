import type { ComputedRef, InjectionKey } from 'vue'

interface FormFieldContext {
  controlId: ComputedRef<string>
  describedBy: ComputedRef<string | undefined>
  invalid: ComputedRef<boolean>
  required: ComputedRef<boolean>
}

export const formFieldKey: InjectionKey<FormFieldContext> = Symbol('form-field')
