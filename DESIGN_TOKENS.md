# Sistema de Design Tokens - DownStream

Este documento descreve o sistema de tokens de design usado no aplicativo DownStream.

## 📐 Sistema de Espaçamento

```rust
SPACING_LARGE  = 16px  // Margens de cards, espaçamento entre seções
SPACING_MEDIUM = 12px  // Entre grupos relacionados (status + metadados)
SPACING_SMALL  = 8px   // Elementos próximos (badges, botões)
SPACING_TINY   = 4px   // Componentes internos (ícone + texto)
```

**Uso:**
- Cards: `margin: SPACING_LARGE` (16px)
- Info box: `spacing: SPACING_MEDIUM` (12px)
- Badges: `spacing: SPACING_SMALL` (8px)
- Tag de chunks: `spacing: SPACING_TINY` (4px)

---

## 🔲 Sistema de Border Radius

```rust
RADIUS_LARGE  = 12px  // Badges de status
RADIUS_MEDIUM = 8px   // Cards principais
RADIUS_SMALL  = 6px   // Grupos de metadados
RADIUS_TINY   = 4px   // Progress bars
```

**Aplicação:**
- `.download-card`: `border-radius: RADIUS_MEDIUM` (8px)
- `.status-badge`: `border-radius: RADIUS_LARGE` (12px)
- `.metadata-group`: `border-radius: RADIUS_SMALL` (6px)
- `.download-progress`: `border-radius: RADIUS_TINY` (4px)

---

## 🎨 Sistema de Cores (Paleta Tailwind)

```rust
COLOR_SUCCESS = #10b981  // Verde (Emerald 500)
COLOR_INFO    = #3b82f6  // Azul (Blue 500)
COLOR_WARNING = #f59e0b  // Âmbar (Amber 500)
COLOR_ERROR   = #ef4444  // Vermelho (Red 500)
COLOR_NEUTRAL = #6b7280  // Cinza (Gray 500)
```

**Mapeamento de Estados:**
| Estado | Cor | Token | Uso |
|--------|-----|-------|-----|
| ✓ Concluído | Verde | `COLOR_SUCCESS` | Downloads completos |
| ⬇ Em progresso | Azul | `COLOR_INFO` | Downloads ativos |
| ⏸ Pausado | Âmbar | `COLOR_WARNING` | Downloads pausados |
| ✕ Falhou | Vermelho | `COLOR_ERROR` | Erros |
| ⊘ Cancelado | Cinza | `COLOR_NEUTRAL` | Downloads cancelados |

---

## 🌫️ Sistema de Opacidade

```rust
OPACITY_BADGE_BG    = 0.15  // Background de badges (15%)
OPACITY_METADATA_BG = 0.03  // Background de metadados (3%)
OPACITY_CARD_BORDER = 0.1   // Bordas de cards (10%)
OPACITY_DIM_TEXT    = 0.75  // Texto secundário (75%)
OPACITY_CANCELLED   = 0.65  // Items cancelados (65%)
```

**Uso:**
- Badges: `background-color: alpha(COLOR, OPACITY_BADGE_BG)`
- Metadados: `background-color: alpha(currentColor, OPACITY_METADATA_BG)`
- Cards: `border: 1px solid alpha(currentColor, OPACITY_CARD_BORDER)`
- Labels secundários: `opacity: OPACITY_DIM_TEXT`
- Downloads cancelados: `opacity: OPACITY_CANCELLED`

---

## 📊 Antes vs Depois

### Antes (Hardcoded)
```css
.status-badge {
    border-radius: 12px;        /* Valor solto */
    padding: 4px 12px;          /* Valores arbitrários */
}

.status-badge.completed {
    background-color: alpha(#10b981, 0.15);  /* Cor hardcoded */
    color: #10b981;
}

.metadata-group {
    padding: 8px 12px;          /* Valores sem sistema */
    border-radius: 6px;         /* Sem consistência */
}
```

### Depois (Com Tokens)
```css
.status-badge {
    border-radius: RADIUS_LARGE;           /* 12px - Token definido */
    padding: SPACING_TINY SPACING_MEDIUM;  /* 4px 12px - Sistema */
}

.status-badge.completed {
    background-color: alpha(COLOR_SUCCESS, OPACITY_BADGE_BG);
    color: COLOR_SUCCESS;
}

.metadata-group {
    padding: SPACING_SMALL SPACING_MEDIUM; /* 8px 12px - Sistema */
    border-radius: RADIUS_SMALL;           /* 6px - Token definido */
}
```

---

## 🎯 Benefícios

### 1. **Consistência Total**
- ✅ Todos os espaçamentos seguem escala de 4px
- ✅ Border radius padronizado em 4 níveis
- ✅ Cores semânticas reutilizáveis

### 2. **Manutenibilidade**
- ✅ Mudar um token atualiza todo o app
- ✅ Fácil criar temas customizados
- ✅ Documentação auto-descritiva

### 3. **Escalabilidade**
- ✅ Adicionar novos estados é trivial
- ✅ Sistema extensível para novas features
- ✅ Base sólida para variações de tema

### 4. **Profissionalismo**
- ✅ Alinhado com padrões da indústria (Tailwind)
- ✅ Design system robusto
- ✅ Código limpo e bem organizado

---

## 🔧 Como Adicionar Novos Tokens

### Exemplo: Adicionar novo estado "Em fila"
```rust
// 1. Adicionar cor (se necessário)
const COLOR_QUEUE: &str = "#8b5cf6";  // Roxo (Violet 500)

// 2. Usar no CSS
.status-badge.queued {
    background-color: alpha(COLOR_QUEUE, OPACITY_BADGE_BG);
    color: COLOR_QUEUE;
}

// 3. Aplicar no código
status_badge.add_css_class("queued");
```

---

## 📚 Referências

- **Paleta de cores**: [Tailwind CSS Colors](https://tailwindcss.com/docs/customizing-colors)
- **Espaçamento**: Sistema baseado em múltiplos de 4px (padrão da indústria)
- **Border radius**: Escala logarítmica (4px, 6px, 8px, 12px)
- **Opacidade**: Valores testados para contraste WCAG AA

---

**Última atualização:** 2025-11-05
**Versão:** 1.0
