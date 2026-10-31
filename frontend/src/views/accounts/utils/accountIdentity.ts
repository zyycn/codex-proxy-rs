import type { Account } from '@/api'

type AccountIdentity = Pick<Account, 'id' | 'email'>
  & Partial<Pick<Account, 'accountId' | 'name' | 'authenticationKind'>>

export function accountIdentityLabel(account: AccountIdentity): string {
  const name = account.name?.trim()
  if (name)
    return name
  const email = account.email?.trim()
  if (email)
    return email
  if (typeof account.accountId === 'string')
    return account.accountId
  return String(account.id)
}
