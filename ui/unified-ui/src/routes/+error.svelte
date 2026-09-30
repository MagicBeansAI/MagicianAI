<script lang="ts">
  import { page } from '$app/stores';

  let status = 404;
  let message = 'SYSTEM_EXCEPTION: Context unresolvable.';

  // Reactively track the page store to ensure we get the right error
  $: {
    if ($page.status) status = $page.status;
    if ($page.error?.message) message = $page.error.message;
  }

  // --- GAME STATE ---
  let activeGame: 'dots' | 'ttt' = 'dots';
  let currentPlayer: 1 | 2 = 1; // 1 = User, 2 = AI
  let scores: Record<number, number> = { 1: 0, 2: 0 };
  let gameOver = false;

  // --- GRID LINK (Dots and Boxes Game) ---
  let gridSize = 5; // default 5x5 dots = 4x4 boxes
  $: dotSize = gridSize === 4 ? 16 : (gridSize === 5 ? 14 : 10);
  
  let hLines: boolean[][] = [];
  let vLines: boolean[][] = [];
  let boxes: (1 | 2 | null)[][] = [];

  function initBoard() {
    hLines = Array(gridSize).fill(null).map(() => Array(gridSize - 1).fill(false));
    vLines = Array(gridSize - 1).fill(null).map(() => Array(gridSize).fill(false));
    boxes = Array(gridSize - 1).fill(null).map(() => Array(gridSize - 1).fill(null));
  }

  // Initialize on mount
  initBoard();

  function resetGame() {
    initBoard();
    scores = { 1: 0, 2: 0 };
    currentPlayer = 1;
    gameOver = false;
  }

  function setGridSize(size: number) {
    gridSize = size;
    resetGame();
  }

  function countLines(r: number, c: number) {
    let count = 0;
    if (hLines[r][c]) count++;
    if (hLines[r+1][c]) count++;
    if (vLines[r][c]) count++;
    if (vLines[r][c+1]) count++;
    return count;
  }

  function checkBoxes() {
    let scored = false;
    for (let r = 0; r < gridSize - 1; r++) {
      for (let c = 0; c < gridSize - 1; c++) {
        if (!boxes[r][c] && hLines[r][c] && hLines[r+1][c] && vLines[r][c] && vLines[r][c+1]) {
          boxes[r][c] = currentPlayer;
          scores[currentPlayer]++;
          scored = true;
        }
      }
    }
    
    if (scores[1] + scores[2] === (gridSize - 1) * (gridSize - 1)) {
      gameOver = true;
    }
    return scored;
  }

  function makeMove(type: 'h' | 'v', r: number, c: number) {
    if (gameOver || currentPlayer !== 1) return;
    
    if (type === 'h' && !hLines[r][c]) {
      hLines[r][c] = true;
      processMove();
    } else if (type === 'v' && !vLines[r][c]) {
      vLines[r][c] = true;
      processMove();
    }
  }

  function processMove() {
    let scored = checkBoxes();
    if (!scored && !gameOver) {
      currentPlayer = 2;
      setTimeout(playAI, 600); // slight delay so it feels like the AI is thinking
    }
  }

  function playAI() {
    if (gameOver || currentPlayer !== 2) return;
    let moveMade = false;
    
    // 1. Offensive: Check if we can score a box right now
    for (let r = 0; r < gridSize - 1; r++) {
      for (let c = 0; c < gridSize - 1; c++) {
        if (!boxes[r][c] && countLines(r, c) === 3) {
          if (!hLines[r][c]) hLines[r][c] = true;
          else if (!hLines[r+1][c]) hLines[r+1][c] = true;
          else if (!vLines[r][c]) vLines[r][c] = true;
          else if (!vLines[r][c+1]) vLines[r][c+1] = true;
          moveMade = true;
          let scored = checkBoxes();
          if (scored && !gameOver) setTimeout(playAI, 400); // AI gets another turn
          return;
        }
      }
    }

    // 2. Defensive: Pick a random safe move that doesn't give away a box
    let safeMoves = [];
    let riskyMoves = [];

    // Check horizontal lines
    for (let r = 0; r < gridSize; r++) {
      for (let c = 0; c < gridSize - 1; c++) {
        if (!hLines[r][c]) {
          let safe = true;
          if (r > 0 && countLines(r - 1, c) === 2) safe = false;
          if (r < gridSize - 1 && countLines(r, c) === 2) safe = false;
          if (safe) safeMoves.push({type: 'h', r, c});
          else riskyMoves.push({type: 'h', r, c});
        }
      }
    }
    // Check vertical lines
    for (let r = 0; r < gridSize - 1; r++) {
      for (let c = 0; c < gridSize; c++) {
        if (!vLines[r][c]) {
          let safe = true;
          if (c > 0 && countLines(r, c - 1) === 2) safe = false;
          if (c < gridSize - 1 && countLines(r, c) === 2) safe = false;
          if (safe) safeMoves.push({type: 'v', r, c});
          else riskyMoves.push({type: 'v', r, c});
        }
      }
    }

    let movePool = safeMoves.length > 0 ? safeMoves : riskyMoves;
    if (movePool.length > 0) {
      let m = movePool[Math.floor(Math.random() * movePool.length)];
      if (m.type === 'h') hLines[m.r][m.c] = true;
      else vLines[m.r][m.c] = true;
      
      let scored = checkBoxes();
      if (!scored && !gameOver) currentPlayer = 1;
      else if (scored && !gameOver) setTimeout(playAI, 400);
    }
  }

  // --- TIC TAC TOE GAME ---
  let tttBoard: (1 | 2 | null)[] = Array(9).fill(null);
  
  function tttReset() {
    tttBoard = Array(9).fill(null);
    scores = { 1: 0, 2: 0 };
    currentPlayer = 1;
    gameOver = false;
  }

  function tttCheckWin(board: (1 | 2 | null)[]) {
    const lines = [
      [0, 1, 2], [3, 4, 5], [6, 7, 8],
      [0, 3, 6], [1, 4, 7], [2, 5, 8],
      [0, 4, 8], [2, 4, 6]
    ];
    for (let i = 0; i < lines.length; i++) {
      const [a, b, c] = lines[i];
      if (board[a] && board[a] === board[b] && board[a] === board[c]) {
        return board[a];
      }
    }
    if (!board.includes(null)) return 'draw';
    return null;
  }

  function tttMove(idx: number) {
    if (gameOver || tttBoard[idx] || currentPlayer !== 1) return;
    tttBoard[idx] = 1;
    
    let winner = tttCheckWin(tttBoard);
    if (winner) {
      gameOver = true;
      if (winner === 1) scores[1]++;
      else if (winner === 2) scores[2]++;
    } else {
      currentPlayer = 2;
      setTimeout(tttAI, 500);
    }
  }

  function tttAI() {
    if (gameOver || currentPlayer !== 2) return;
    
    let move = -1;
    const checkMove = (player: 1 | 2) => {
      for (let i = 0; i < 9; i++) {
        if (!tttBoard[i]) {
          let b = [...tttBoard];
          b[i] = player;
          if (tttCheckWin(b) === player) return i;
        }
      }
      return -1;
    }
    
    move = checkMove(2);
    if (move === -1) move = checkMove(1);
    if (move === -1 && !tttBoard[4]) move = 4;
    if (move === -1) {
      let empty = tttBoard.map((v, i) => v === null ? i : -1).filter(i => i !== -1);
      if (empty.length > 0) move = empty[Math.floor(Math.random() * empty.length)];
    }
    
    if (move !== -1) {
      tttBoard[move] = 2;
      let winner = tttCheckWin(tttBoard);
      if (winner) {
        gameOver = true;
        if (winner === 1) scores[1]++;
        else if (winner === 2) scores[2]++;
      } else {
        currentPlayer = 1;
      }
    }
  }

  function switchGame(game: 'dots' | 'ttt') {
    if (activeGame === game) return;
    activeGame = game;
    if (game === 'dots') resetGame();
    else tttReset();
  }
</script>

<div class="error-grid">
  <!-- Glowing ambient background effect -->
  <div class="ambient-glow"></div>

  <div class="error-panel">
    <div class="top-row">
      <div class="brand-lockup">
        <span class="logo">magican</span>
      </div>
      <div class="status-code">{status}</div>
    </div>

    <div class="message">
      <div class="sys-label">SYSTEM_ANOMALY_DETECTED</div>
      {#if status === 404}
        <p>The requested trajectory or sector could not be found within the current fleet topology.</p>
      {:else}
        <p>A critical fault occurred during orchestrator execution. The fleet has logged this anomaly.</p>
      {/if}
    </div>

    <!-- MINI GAMES UI -->
    <div class="game-container">
      <div class="carousel-nav">
        <button on:click={() => switchGame('dots')} class:active={activeGame === 'dots'}>&lt;</button>
        <span class="game-title">{activeGame === 'dots' ? 'DOTS & BOXES' : 'TIC TAC TOE'}</span>
        <button on:click={() => switchGame('ttt')} class:active={activeGame === 'ttt'}>&gt;</button>
      </div>

      <div class="game-status-top">
        {#if gameOver}
          <div class="game-over-row">
            <span class="game-over-text">
              {#if scores[1] > scores[2]} SYSTEM OVERRIDE SUCCESS!
              {:else if scores[2] > scores[1]} ANOMALY WINS.
              {:else} STALEMATE. {/if}
            </span>
            <button class="btn-reset" on:click={activeGame === 'dots' ? resetGame : tttReset}>REBOOT SIM</button>
          </div>
        {:else}
          <div class="turn-indicator">
            {currentPlayer === 1 ? 'YOUR TURN' : 'SYSTEM THINKING...'}
          </div>
        {/if}
      </div>

      <div class="game-layout">
        <div class="side-score">
          <div class="score-card player1" class:active={currentPlayer === 1}>
            <span class="label">OPERATOR</span>
            <span class="score">{scores[1]}</span>
          </div>
        </div>

        <div class="board-wrapper">
          {#if activeGame === 'dots'}
            <div 
              class="board" 
              style="
                --dot-size: {dotSize}px;
                grid-template-columns: repeat({gridSize - 1}, var(--dot-size) 1fr) var(--dot-size); 
                grid-template-rows: repeat({gridSize - 1}, var(--dot-size) 1fr) var(--dot-size);
              "
            >
              {#each Array(gridSize * 2 - 1) as _, rowIdx}
                {#each Array(gridSize * 2 - 1) as __, colIdx}
                  
                  <!-- DOT (Even Row, Even Col) -->
                  {#if rowIdx % 2 === 0 && colIdx % 2 === 0}
                    <div class="dot"></div>
                  {/if}

                  <!-- HORIZONTAL LINE (Even Row, Odd Col) -->
                  {#if rowIdx % 2 === 0 && colIdx % 2 !== 0}
                    {@const r = rowIdx / 2}
                    {@const c = Math.floor(colIdx / 2)}
                    <button 
                      class="line h-line" 
                      class:active={hLines[r][c]} 
                      on:click={() => makeMove('h', r, c)}
                      disabled={hLines[r][c] || currentPlayer !== 1 || gameOver}
                      aria-label="Draw horizontal line"
                    ></button>
                  {/if}

                  <!-- VERTICAL LINE (Odd Row, Even Col) -->
                  {#if rowIdx % 2 !== 0 && colIdx % 2 === 0}
                    {@const r = Math.floor(rowIdx / 2)}
                    {@const c = colIdx / 2}
                    <button 
                      class="line v-line" 
                      class:active={vLines[r][c]} 
                      on:click={() => makeMove('v', r, c)}
                      disabled={vLines[r][c] || currentPlayer !== 1 || gameOver}
                      aria-label="Draw vertical line"
                    ></button>
                  {/if}

                  <!-- BOX (Odd Row, Odd Col) -->
                  {#if rowIdx % 2 !== 0 && colIdx % 2 !== 0}
                    {@const r = Math.floor(rowIdx / 2)}
                    {@const c = Math.floor(colIdx / 2)}
                    <div class="box" class:p1={boxes[r][c] === 1} class:p2={boxes[r][c] === 2}>
                      {#if boxes[r][c] === 1}
                        O
                      {:else if boxes[r][c] === 2}
                        S
                      {/if}
                    </div>
                  {/if}

                {/each}
              {/each}
            </div>
          {:else}
            <div class="ttt-board">
              {#each Array(9) as _, i}
                <button 
                  class="ttt-cell" 
                  class:p1={tttBoard[i] === 1} 
                  class:p2={tttBoard[i] === 2}
                  disabled={tttBoard[i] !== null || currentPlayer !== 1 || gameOver}
                  on:click={() => tttMove(i)}
                >
                  {#if tttBoard[i] === 1}X{:else if tttBoard[i] === 2}O{/if}
                </button>
              {/each}
            </div>
          {/if}
        </div>

      <div class="side-score">
        <div class="score-card player2" class:active={currentPlayer === 2}>
          <span class="label">SYSTEM</span>
          <span class="score">{scores[2]}</span>
        </div>
      </div>
    </div>
  </div>

  <div class="actions">
    <div class="size-selector" style="visibility: {activeGame === 'dots' ? 'visible' : 'hidden'}">
      <span class="size-label">GRID SIZE</span>
      <button class:active={gridSize === 4} on:click={() => setGridSize(4)}>3x3</button>
      <button class:active={gridSize === 5} on:click={() => setGridSize(5)}>4x4</button>
      <button class:active={gridSize === 7} on:click={() => setGridSize(7)}>6x6</button>
    </div>
    <a href="/" class="btn-return">
      <span class="icon">↤</span> Return to Operations
    </a>
  </div>
  </div>
</div>

<style>
  .error-grid {
    position: fixed;
    top: 0;
    left: 0;
    width: 100vw;
    height: 100vh;
    display: flex;
    align-items: center;
    justify-content: center;
    background-color: var(--bg-base);
    color: var(--text-primary);
    overflow: hidden;
    font-family: var(--font-primary);
    z-index: 9999;
  }

  .ambient-glow {
    position: absolute;
    top: 50%;
    left: 50%;
    width: 60vw;
    height: 60vw;
    transform: translate(-50%, -50%);
    background: radial-gradient(circle, var(--accent-primary-soft) 0%, transparent 70%);
    opacity: 0.5;
    pointer-events: none;
    z-index: 0;
  }

  .error-panel {
    position: relative;
    z-index: 1;
    display: flex;
    flex-direction: column;
    width: 95%;
    max-width: 640px;
    padding: clamp(0.75rem, 2vh, 1.25rem) clamp(1.25rem, 4vw, 2.5rem);
    background-color: var(--bg-surface);
    border: 1px solid var(--border-soft);
    border-radius: 24px;
    box-shadow: var(--shadow-lg);
    backdrop-filter: blur(20px);
    animation: fade-up 0.5s var(--spring-pop);
  }

  @keyframes fade-up {
    from { opacity: 0; transform: translateY(20px); }
    to { opacity: 1; transform: translateY(0); }
  }

  .top-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 0.5rem;
    border-bottom: 1px solid var(--border-soft);
    padding-bottom: 0.375rem;
  }

  .brand-lockup {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
  }

  .logo {
    font-family: var(--font-brand);
    font-size: 1.75rem;
    font-weight: 700;
    line-height: 1;
    color: var(--text-primary);
  }

  .status-code {
    font-family: var(--font-mono);
    font-size: var(--text-xl);
    font-weight: 800;
    line-height: 1;
    color: var(--color-error);
    text-shadow: 0 0 10px var(--color-error-soft);
  }

  .message {
    margin-bottom: 0;
  }

  .sys-label {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    letter-spacing: 0.05em;
    color: var(--color-error);
    margin-bottom: 0.5rem;
  }

  .message p {
    font-size: var(--text-sm);
    color: var(--text-secondary);
    line-height: var(--leading-relaxed);
    margin: 0;
  }

  /* GAME CSS */
  .game-container {
    margin-top: 0.75rem;
    background: var(--bg-surface-elevated);
    border: 1px solid var(--border-soft);
    border-radius: 12px;
    padding: 1rem;
    margin-bottom: 0.5rem;
    box-shadow: inset 0 2px 10px rgba(0,0,0,0.02);
    display: flex;
    flex-direction: column;
    height: 460px;
    max-height: 60vh;
    min-height: 0;
  }

  .carousel-nav {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 0;
    padding-bottom: 0.5rem;
    border-bottom: 1px dashed var(--border-soft);
  }
  .carousel-nav button {
    background: transparent;
    border: none;
    color: var(--text-secondary);
    font-size: 1.5rem;
    cursor: pointer;
    transition: color 0.2s;
  }
  .carousel-nav button:hover {
    color: var(--text-primary);
  }
  .game-title {
    font-family: var(--font-display);
    font-weight: bold;
    letter-spacing: 2px;
    font-size: 1rem;
    color: var(--text-primary);
  }

  .game-status-top {
    display: flex;
    flex-direction: column;
    align-items: center;
    margin-bottom: 0.25rem;
    font-family: var(--font-mono);
    min-height: 24px;
    justify-content: center;
    line-height: 1;
  }

  .game-layout {
    display: flex;
    justify-content: center;
    align-items: center;
    gap: 1.5rem;
    flex: 1 1 auto;
    min-height: 0;
  }

  .side-score {
    flex: 0 0 auto;
    width: 80px;
    display: flex;
    justify-content: center;
  }

  .score-card {
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 0.75rem 0.5rem;
    border-radius: 8px;
    transition: all 0.3s;
    opacity: 0.4;
    width: 100%;
  }
  .score-card.active {
    opacity: 1;
    transform: scale(1.05);
  }

  .player1.active { color: var(--color-info); }
  .player2.active { color: var(--color-error); }

  .score-card .label { font-size: 10px; letter-spacing: 1px; }
  .score-card .score { font-size: 28px; font-weight: bold; margin-top: 0.25rem; }

  .turn-indicator {
    font-size: var(--text-xs);
    letter-spacing: 1px;
    line-height: 1;
    color: var(--text-secondary);
  }
  .game-over-row {
    display: flex;
    align-items: center;
    gap: 1rem;
  }
  .game-over-text {
    font-size: var(--text-sm);
    font-weight: bold;
    color: var(--text-primary);
  }
  .btn-reset {
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 6px 12px;
    background: var(--bg-surface);
    border: 1px solid var(--border-soft);
    border-radius: 4px;
    cursor: pointer;
    transition: all 0.2s;
  }
  .btn-reset:hover { background: var(--bg-elevated); border-color: var(--text-primary); }

  .board-wrapper {
    display: flex;
    justify-content: center;
    align-items: center;
    flex: 1;
    align-self: stretch;
    min-width: 0;
    min-height: 0;
    container-type: size;
  }

  .board {
    display: grid;
    gap: 0;
    width: min(100cqi, 100cqb, 320px);
    height: min(100cqi, 100cqb, 320px);
    aspect-ratio: 1;
  }

  .dot {
    width: var(--dot-size);
    height: var(--dot-size);
    background-color: var(--text-muted);
    border-radius: 50%;
    box-shadow: 0 0 4px var(--bg-base);
    z-index: 2;
  }

  .line {
    background-color: var(--border-soft);
    border: none;
    cursor: pointer;
    transition: all 0.2s;
    padding: 0;
    margin: 0;
    z-index: 1;
  }
  .line:hover:not(.disabled) {
    background-color: var(--border-default);
  }
  
  .h-line {
    width: 100%;
    height: 100%;
  }
  
  .v-line {
    width: 100%;
    height: 100%;
  }

  /* Player colors for lines */
  .line.active { cursor: default; }
  .h-line.active { background-color: var(--text-primary); box-shadow: 0 0 5px var(--text-faint); }
  .v-line.active { background-color: var(--text-primary); box-shadow: 0 0 5px var(--text-faint); }

  .size-selector {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .size-label {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--text-muted);
    letter-spacing: 1px;
    margin-right: 0.25rem;
  }
  .size-selector button {
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 6px 10px;
    background: var(--bg-surface);
    border: 1px solid var(--border-soft);
    border-radius: 4px;
    cursor: pointer;
    color: var(--text-secondary);
    transition: all 0.2s;
  }
  .size-selector button:hover {
    background: var(--bg-elevated);
    border-color: var(--text-primary);
    color: var(--text-primary);
  }
  .size-selector button.active {
    background: var(--text-primary);
    color: var(--bg-base);
    border-color: var(--text-primary);
  }

  .box {
    width: 100%;
    height: 100%;
    transition: background-color 0.3s, color 0.3s;
    display: flex;
    align-items: center;
    justify-content: center;
    font-family: var(--font-display);
    font-weight: 800;
    font-size: 1.25rem;
  }
  .box.p1 { background-color: var(--color-info-soft); color: var(--color-info); }
  .box.p2 { background-color: var(--color-error-soft); color: var(--color-error); }

  .actions {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-top: 1rem;
  }

  .btn-return {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.75rem 1.5rem;
    background-color: var(--accent-primary);
    color: var(--text-on-accent);
    font-family: var(--font-display);
    font-size: var(--text-sm);
    font-weight: 600;
    text-decoration: none;
    border-radius: 12px;
    transition: all 0.2s var(--ease-settle);
  }

  .btn-return:hover {
    transform: translateY(-2px);
    box-shadow: var(--shadow-glow);
    background-color: var(--accent-primary-hover);
  }

  /* TIC TAC TOE CSS */
  .ttt-board {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    grid-template-rows: repeat(3, 1fr);
    gap: 8px;
    width: min(100cqi, 100cqb, 320px);
    height: min(100cqi, 100cqb, 320px);
    aspect-ratio: 1;
  }
  .ttt-cell {
    background-color: var(--border-soft);
    border: none;
    border-radius: 8px;
    font-family: var(--font-display);
    font-size: 3rem;
    font-weight: bold;
    cursor: pointer;
    transition: background-color 0.2s, transform 0.1s;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .ttt-cell:hover:not(:disabled) {
    background-color: var(--border-default);
    transform: scale(1.05);
  }
  .ttt-cell.p1 { color: var(--color-info); }
  .ttt-cell.p2 { color: var(--color-error); }
</style>
