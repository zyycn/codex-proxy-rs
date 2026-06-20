<script setup lang="ts">
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { ArrowRight, KeyRound, Mail, SquareTerminal } from '@lucide/vue'

import BaseCard from '@/components/base/BaseCard.vue'
import { useAuthStore } from '@/stores/modules/auth'

const router = useRouter()
const authStore = useAuthStore()

const username = ref('')
const password = ref('')

async function handleSubmit() {
  if (!username.value.trim() || !password.value.trim()) {
    return
  }

  const success = await authStore.login({
    username: username.value.trim(),
    password: password.value,
  })

  if (success) {
    router.push('/')
  }
}
</script>

<template>
  <main
    class="flex h-screen w-screen items-center justify-center overflow-hidden bg-[linear-gradient(115deg,#F8FAFC_0%,#FFFFFF_46%,#F1F7FF_100%)] px-6 py-8 min-[961px]:px-24 min-[961px]:py-12"
  >
    <section
      class="grid h-full max-h-[calc(100vh-64px)] w-full max-w-[1728px] items-center gap-8 min-[961px]:max-h-[calc(100vh-96px)] min-[961px]:grid-cols-[minmax(0,980px)_520px] min-[961px]:gap-22"
      aria-label="Codex Proxy RS 登录"
    >
      <div class="relative h-full min-h-0 overflow-hidden">
        <div class="absolute left-0 top-0 flex h-13 w-[420px] items-center gap-3.25">
          <span class="inline-flex size-11 items-center justify-center rounded-[13px] bg-[#111827] text-white">
            <SquareTerminal :size="22" />
          </span>
          <span class="grid h-10.5 content-center gap-1">
            <strong class="text-[17px] leading-[1.1] font-[760] text-[#0E1726]">Codex Proxy RS</strong>
            <span class="text-[11px] leading-[1.1] font-[650] text-[#64748B]">Proxy RS · v0.1.0</span>
          </span>
        </div>

        <div class="pointer-events-none absolute left-2 top-[26%] h-[238px] w-[910px] max-w-[96vw]">
          <svg class="size-full overflow-visible" viewBox="0 0 910 238" fill="none" preserveAspectRatio="none">
            <path
              d="M0 134 C154 30 304 54 440 116 C592 186 726 178 910 54"
              stroke="#2563EB"
              stroke-linecap="round"
              stroke-linejoin="round"
              stroke-width="2.2"
            />
          </svg>
        </div>

        <div class="pointer-events-none absolute left-6 top-[35%] h-[210px] w-[870px] max-w-[92vw]">
          <svg class="size-full overflow-visible" viewBox="0 0 870 210" fill="none" preserveAspectRatio="none">
            <path
              d="M0 62 C138 150 286 154 424 94 C580 26 708 48 870 150"
              stroke="#0F9F9A"
              stroke-linecap="round"
              stroke-linejoin="round"
              stroke-width="2"
            />
          </svg>
        </div>

        <div class="pointer-events-none absolute left-10.5 top-[30%] h-[210px] w-[820px] max-w-[88vw] opacity-70">
          <svg class="size-full overflow-visible" viewBox="0 0 820 210" fill="none" preserveAspectRatio="none">
            <path
              d="M0 118 C132 54 266 76 392 124 C540 178 666 156 820 86"
              stroke="#93C5FD"
              stroke-linecap="round"
              stroke-linejoin="round"
              stroke-width="1.2"
            />
          </svg>
        </div>

        <div class="pointer-events-none absolute left-15 top-[45%] h-[178px] w-[780px] max-w-[84vw] opacity-70">
          <svg class="size-full overflow-visible" viewBox="0 0 780 178" fill="none" preserveAspectRatio="none">
            <path
              d="M0 78 C118 42 224 118 352 78 C500 32 632 96 780 62"
              stroke="#99F6E4"
              stroke-linecap="round"
              stroke-linejoin="round"
              stroke-width="1.2"
            />
          </svg>
        </div>

        <div class="pointer-events-none absolute left-24 top-[56%] h-[130px] w-[680px] max-w-[74vw] opacity-60">
          <svg class="size-full overflow-visible" viewBox="0 0 680 130" fill="none" preserveAspectRatio="none">
            <path
              d="M0 62 C108 88 192 30 316 58 C442 86 548 42 680 66"
              stroke="#CBD5E1"
              stroke-linecap="round"
              stroke-linejoin="round"
              stroke-width="1.1"
            />
          </svg>
        </div>

        <span class="pointer-events-none absolute left-[446px] top-[39%] size-1.75 rounded-full bg-[#2563EB] shadow-[0_0_14px_#2563EB66]" />
        <span class="pointer-events-none absolute left-[602px] top-[47%] size-1.75 rounded-full bg-[#0F9F9A] shadow-[0_0_14px_#0F9F9A66]" />
        <span class="pointer-events-none absolute left-[262px] top-[35%] size-1 rounded-full bg-[#94A3B8]" />

        <div class="absolute bottom-8 left-0 grid w-[580px] max-w-full gap-3.5 min-[961px]:bottom-[64px] min-[961px]:left-14">
          <h1 class="m-0 text-[38px] leading-[1.08] font-[780] tracking-normal text-[#0E1726]">
            安静进入控制台。
          </h1>
          <p class="m-0 max-w-[580px] text-sm leading-[1.45] font-medium text-[#64748B]">
            几条请求轨迹，连接账号池、模型目录与管理入口。
          </p>
        </div>
      </div>

      <BaseCard
        as="form"
        :padded="false"
        radius-class="rounded-[22px]"
        shadow-class="shadow-[0_12px_26px_-18px_rgba(14,23,38,0.122),0_34px_70px_-40px_rgba(14,23,38,0.161)]"
        class="grid w-full max-w-120 gap-5.5 p-9"
        @submit.prevent="handleSubmit"
      >
        <header class="grid h-21 gap-2.5">
          <h2 class="m-0 text-[34px] leading-[1.12] font-extrabold tracking-normal text-[#0E1726]">
            登录控制台
          </h2>
          <p class="m-0 text-sm leading-[1.55] font-medium text-[#64748B]">
            使用管理员账号和访问密钥进入 Codex Proxy RS。
          </p>
        </header>

        <div v-if="authStore.error" class="px-4 py-3 rounded-xl bg-red-50 border border-red-200">
          <p class="m-0 text-[13px] font-medium text-red-700">
            {{ authStore.error }}
          </p>
        </div>

        <label class="grid h-18 gap-2">
          <span class="text-xs leading-[1.1] font-bold text-[#0E1726]">管理员账号</span>
          <span class="inline-flex h-11.5 items-center gap-2.5 rounded-xl bg-[#EEF2F7] px-3.5 text-[#64748B] shadow-[inset_0_1px_0_#FFFFFF66] transition focus-within:bg-[#F8FAFC] focus-within:shadow-[inset_0_1px_0_#FFFFFF,0_0_0_3px_#DBEAFE]">
            <Mail :size="17" />
            <input
              v-model="username"
              class="min-w-0 flex-1 border-0 bg-transparent text-[13px] leading-none font-medium text-[#0E1726] outline-0 placeholder:text-[#94A3B8]"
              placeholder="请输入用户名"
              type="text"
              required
            >
          </span>
        </label>

        <label class="grid h-18 gap-2">
          <span class="text-xs leading-[1.1] font-bold text-[#0E1726]">访问密钥</span>
          <span class="inline-flex h-11.5 items-center gap-2.5 rounded-xl bg-[#EEF2F7] px-3.5 text-[#64748B] shadow-[inset_0_1px_0_#FFFFFF66] transition focus-within:bg-[#F8FAFC] focus-within:shadow-[inset_0_1px_0_#FFFFFF,0_0_0_3px_#DBEAFE]">
            <KeyRound :size="17" />
            <input
              v-model="password"
              class="min-w-0 flex-1 border-0 bg-transparent text-[13px] leading-none font-medium text-[#0E1726] outline-0 placeholder:text-[#94A3B8]"
              placeholder="输入会话密钥"
              type="password"
              required
              @keyup.enter="handleSubmit"
            >
          </span>
        </label>

        <button
          class="inline-flex h-12 items-center justify-center gap-2.5 rounded-xl border-0 bg-[#2563EB] text-[15px] leading-[1.1] font-[760] text-white shadow-[0_12px_24px_-16px_#2563EB66] transition-colors hover:bg-[#1D4ED8] disabled:opacity-50 disabled:cursor-not-allowed"
          type="submit"
          :disabled="authStore.loading || !username.trim() || !password.trim()"
        >
          <span>{{ authStore.loading ? '登录中...' : '登录' }}</span>
          <ArrowRight v-if="!authStore.loading" :size="18" />
        </button>
      </BaseCard>
    </section>
  </main>
</template>
