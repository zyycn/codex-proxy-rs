import { isRecord } from '@/utils/data'

export interface InputField {
  key: string
  label: string
  description?: string
  required: boolean
  type: 'string' | 'number' | 'integer' | 'boolean'
  secret: boolean
  values?: Array<string | number | boolean | null>
}

// 仅将能无损编辑的平面字段映射为控件，复杂 schema 保留 JSON 编辑，完整校验由服务端负责。
export function inputFields(schema: Record<string, unknown>): InputField[] | null {
  if (['$ref', 'allOf', 'anyOf', 'oneOf', 'if', 'dependentSchemas'].some(key => key in schema))
    return null
  if (!isRecord(schema.properties))
    return schema.additionalProperties === false ? [] : null
  const required = Array.isArray(schema.required) ? schema.required : []
  const fields: InputField[] = []
  for (const [key, value] of Object.entries(schema.properties)) {
    if (!isRecord(value) || !['string', 'number', 'integer', 'boolean'].includes(String(value.type)))
      return null
    if (['$ref', 'allOf', 'anyOf', 'oneOf', 'const', 'readOnly'].some(key => key in value))
      return null
    const values = value.enum
    if (values !== undefined && (!Array.isArray(values) || !values.every(item => item === null || ['string', 'number', 'boolean'].includes(typeof item))))
      return null
    fields.push({
      key,
      label: typeof value.title === 'string' ? value.title : key,
      description: typeof value.description === 'string' ? value.description : undefined,
      type: value.type as InputField['type'],
      required: required.includes(key),
      secret: value.writeOnly === true || value.format === 'password',
      values: values as InputField['values'],
    })
  }
  return fields.length || schema.additionalProperties === false ? fields : null
}
