<script setup lang="ts">
import { LockKeyhole } from '@lucide/vue'
import { shallowRef } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import ChangePasswordModal from './ChangePasswordModal.vue'

const open = shallowRef(false)
</script>

<template>
  <BaseCard title="管理员密码" description="用于登录管理后台">
    <template #actions>
      <BaseButton variant="secondary" @click="open = true">
        <template #icon>
          <LockKeyhole class="size-4" />
        </template>
        修改密码
      </BaseButton>
    </template>
    <p class="m-0 text-cp leading-relaxed text-cp-text-secondary">
      修改后，所有已登录的管理员都需要使用新密码重新登录。
    </p>
  </BaseCard>
  <ChangePasswordModal v-model="open" />
</template>
