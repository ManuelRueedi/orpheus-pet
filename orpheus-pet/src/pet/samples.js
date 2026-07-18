// Localized lines the witch speaks, matched to the selected voice's language.
//
// Every voice belongs to a language (VOICE_LANG); LINES[lang] holds four sets:
//   samples   - random line on idle left-click
//   greetings - spoken on voice change; "{name}" -> the voice's name
//   hellos    - spoken when the pet is shown
//   goodbyes  - spoken when the pet is hidden
// main.js falls back to English if a language/set is missing. Add a language
// by adding its voices to VOICE_LANG and a LINES entry with the same shape.

// Voice -> language, mirroring Orpheus-FastAPI's voice groups
// (tts_engine/inference.py).
export const VOICE_LANG = {
  // English
  tara: "en", leah: "en", jess: "en", leo: "en", dan: "en", mia: "en", zac: "en", zoe: "en",
  // French
  pierre: "fr", amelie: "fr", marie: "fr",
  // German
  jana: "de", thomas: "de", max: "de",
  // Korean
  "유나": "ko", "준서": "ko",
  // Hindi
  "ऋतिका": "hi",
  // Mandarin
  "长乐": "zh", "白芷": "zh",
  // Spanish
  javi: "es", sergio: "es", maria: "es",
  // Italian
  pietro: "it", giulia: "it", carlo: "it",
};

export const LINES = {
  en: {
    samples: [
      "Oh, hello there! Click me while I'm talking to pause — then click again and I'll pick up right where I left off.",
      "Highlight any text on your screen and press your read-aloud shortcut. I'll read it out loud in whatever voice you fancy.",
      "Right-click me to open my little spellbook, where you can type something for me to say.",
      "The cauldron's bubbling and the moon is high — I've got all night to chat with you.",
      "I can speak in twenty-five different voices. Go on, pick a new one and click me again.",
      "Drag me anywhere you like. I'll perch in the corner of your screen and keep you company.",
      "Careful what you leave lying around on your clipboard… I just might read it out loud.",
      "Every good witch needs a familiar. Today, that's you. Hello, familiar!",
      "Feeling stuck? Sometimes saying a thing out loud is the first spell that actually works.",
      "That's a little taste of my magic. Now highlight some real text and let's try it for real.",
      "I put a spell on you… to make you smile. Did it work? No? Hang on, let me try that again.",
      "They say I'm a little batty. Rude. My bats are perfectly well behaved, thank you.",
      "I'd offer you a potion, but the last one turned my roommate into a houseplant. He's thriving, honestly.",
      "Knock knock. Who's there? A witch. A witch who? A witch you keep clicking on, apparently.",
      "I could turn you into a frog, but you seem far more fun as a person. For now.",
      "My favorite spell is the one that makes Mondays disappear. Still workshopping it.",
      "Fun fact: black cats are just tiny wizards who chose a simpler life.",
      "I don't always read your clipboard… but when I do, I hope it's something juicy.",
      "Careful — I've had three espressos and a mystery potion. Truly anything could happen.",
      "I've studied the ancient scrolls, the forbidden tomes, and roughly four thousand of your browser tabs.",
      "Abracadabra, alakazoo — I still can't believe someone actually coded me. Hi there!",
      "You can drag me around all day, but my heart stays right here, on your taskbar.",
      "Spell check? Darling, I AM the spell check.",
      "Some witches collect eye of newt. I collect awkward silences. Care to donate one?",
      "Warning: excessive clicking may result in excessive charm. You have been warned.",
      "If a witch talks on your desktop and nobody turns up their volume… does she make a sound?",
    ],
    greetings: [
      "Hi there, I'm {name}!",
      "{name}, at your service.",
      "Ooh, you've switched to {name}. Hello!",
      "This is {name}. What shall we say?",
      "Hey, it's {name}. Ready when you are.",
      "Now you're hearing {name}. Hi!",
    ],
    hellos: [
      "I'm back!",
      "Did you miss me?",
      "Hello again!",
      "Ta-da!",
      "Peekaboo!",
      "Reporting for spell duty.",
    ],
    goodbyes: [
      "See you soon!",
      "Off I pop!",
      "Bye for now!",
      "I'll be in the tray if you need me.",
      "Poof! Gone.",
      "Toodles!",
    ],
  },

  fr: {
    samples: [
      "Bonjour ! Cliquez sur moi pendant que je parle pour mettre en pause, puis cliquez encore pour reprendre.",
      "Surlignez n'importe quel texte à l'écran et appuyez sur votre raccourci — je le lirai à voix haute.",
      "Faites un clic droit sur moi pour ouvrir mon petit grimoire et taper ce que je dois dire.",
      "Le chaudron bout et la lune est haute — j'ai toute la nuit pour bavarder avec vous.",
      "Je peux parler avec vingt-cinq voix différentes. Allez, choisissez-en une nouvelle.",
      "Attention à ce que vous laissez dans votre presse-papiers… je pourrais bien le lire à voix haute.",
      "Chaque bonne sorcière a besoin d'un familier. Aujourd'hui, c'est vous. Bonjour, familier !",
      "Vérification orthographique ? Mon cher, la magie des mots, c'est moi.",
    ],
    greetings: [
      "Bonjour, je suis {name} !",
      "{name}, à votre service.",
      "Vous avez choisi {name}. Enchantée !",
      "Ici {name}. Que dirons-nous ?",
      "Coucou, c'est {name}. Prête quand vous voulez.",
    ],
    hellos: ["Me revoilà !", "Je vous ai manqué ?", "Coucou !", "Et voilà !", "Prête pour le service magique."],
    goodbyes: ["À bientôt !", "Je file !", "Au revoir pour l'instant !", "Je serai dans la barre si besoin.", "Pouf ! Disparue."],
  },

  de: {
    samples: [
      "Hallo! Klick mich an, während ich spreche, um zu pausieren, und klick erneut, um fortzufahren.",
      "Markiere beliebigen Text auf dem Bildschirm und drück dein Tastenkürzel — ich lese ihn vor.",
      "Mach einen Rechtsklick auf mich, um mein kleines Zauberbuch zu öffnen und etwas einzutippen.",
      "Der Kessel brodelt und der Mond steht hoch — ich habe die ganze Nacht Zeit zum Plaudern.",
      "Ich kann mit fünfundzwanzig verschiedenen Stimmen sprechen. Los, wähl eine neue aus.",
      "Pass auf, was du in der Zwischenablage liegen lässt… ich könnte es vorlesen.",
      "Jede gute Hexe braucht einen Vertrauten. Heute bist das du. Hallo, Vertrauter!",
      "Rechtschreibprüfung? Liebes, der Zauber der Worte, das bin ich.",
    ],
    greetings: [
      "Hallo, ich bin {name}!",
      "{name}, zu deinen Diensten.",
      "Du hast {name} gewählt. Freut mich!",
      "Hier ist {name}. Was sagen wir?",
      "Hey, hier {name}. Bereit, wenn du es bist.",
    ],
    hellos: ["Ich bin wieder da!", "Hast du mich vermisst?", "Kuckuck!", "Tada!", "Zum Zauberdienst bereit."],
    goodbyes: ["Bis bald!", "Ich verschwinde!", "Tschüss für jetzt!", "Ich bin im Tray, falls du mich brauchst.", "Puff! Weg."],
  },

  es: {
    samples: [
      "¡Hola! Haz clic en mí mientras hablo para pausar, y clic de nuevo para continuar.",
      "Resalta cualquier texto de la pantalla y pulsa tu atajo — lo leeré en voz alta.",
      "Haz clic derecho en mí para abrir mi pequeño libro de hechizos y escribir algo.",
      "El caldero burbujea y la luna está alta — tengo toda la noche para charlar contigo.",
      "Puedo hablar con veinticinco voces distintas. Venga, elige una nueva.",
      "Cuidado con lo que dejas en el portapapeles… podría leerlo en voz alta.",
      "Toda buena bruja necesita un familiar. Hoy, ese eres tú. ¡Hola, familiar!",
      "¿Corrector ortográfico? Cariño, la magia de las palabras soy yo.",
    ],
    greetings: [
      "¡Hola, soy {name}!",
      "{name}, a tu servicio.",
      "Has elegido a {name}. ¡Encantada!",
      "Aquí {name}. ¿Qué decimos?",
      "Ey, soy {name}. Lista cuando quieras.",
    ],
    hellos: ["¡Ya estoy de vuelta!", "¿Me echaste de menos?", "¡Cucú!", "¡Tachán!", "Lista para el servicio mágico."],
    goodbyes: ["¡Hasta pronto!", "¡Me esfumo!", "¡Adiós por ahora!", "Estaré en la bandeja si me necesitas.", "¡Puf! Desaparecí."],
  },

  it: {
    samples: [
      "Ciao! Cliccami mentre parlo per mettere in pausa, e clicca di nuovo per riprendere.",
      "Evidenzia un testo qualsiasi sullo schermo e premi la tua scorciatoia — lo leggerò ad alta voce.",
      "Fai clic destro su di me per aprire il mio piccolo libro degli incantesimi e scrivere qualcosa.",
      "Il calderone ribolle e la luna è alta — ho tutta la notte per chiacchierare con te.",
      "So parlare con venticinque voci diverse. Dai, scegline una nuova.",
      "Attento a cosa lasci negli appunti… potrei leggerlo ad alta voce.",
      "Ogni brava strega ha bisogno di un famiglio. Oggi sei tu. Ciao, famiglio!",
      "Correttore ortografico? Tesoro, la magia delle parole sono io.",
    ],
    greetings: [
      "Ciao, sono {name}!",
      "{name}, al tuo servizio.",
      "Hai scelto {name}. Piacere!",
      "Qui {name}. Cosa diciamo?",
      "Ehi, sono {name}. Pronta quando vuoi.",
    ],
    hellos: ["Sono tornata!", "Ti sono mancata?", "Cucù!", "Tadà!", "Pronta per il servizio magico."],
    goodbyes: ["A presto!", "Sparisco!", "Ciao per ora!", "Sarò nella barra se ti servo.", "Puf! Sparita."],
  },

  ko: {
    samples: [
      "안녕하세요! 제가 말하는 동안 저를 클릭하면 멈추고, 다시 클릭하면 이어서 말해요.",
      "화면의 아무 텍스트나 선택하고 단축키를 누르면 제가 소리 내어 읽어드릴게요.",
      "저를 오른쪽 클릭하면 작은 마법책이 열려요. 하고 싶은 말을 적어보세요.",
      "가마솥이 보글보글 끓고 달도 높이 떴네요. 밤새 이야기할 시간이 충분해요.",
      "저는 스물다섯 가지 목소리로 말할 수 있어요. 새로운 목소리를 골라보세요.",
      "클립보드에 남겨둔 걸 조심하세요… 제가 소리 내어 읽을지도 몰라요.",
      "좋은 마녀에게는 친구가 필요해요. 오늘은 바로 당신이네요. 안녕, 내 친구!",
      "맞춤법 검사요? 제가 바로 말의 마법이랍니다.",
    ],
    greetings: [
      "안녕하세요, 저는 {name}입니다!",
      "{name}, 대령했습니다.",
      "{name} 목소리로 바꾸셨네요. 반가워요!",
      "여기는 {name}입니다. 무슨 말을 해볼까요?",
      "안녕하세요! {name} 목소리예요.",
    ],
    hellos: ["돌아왔어요!", "보고 싶었어요?", "까꿍!", "짜잔!", "마법 근무 준비 완료!"],
    goodbyes: ["곧 또 만나요!", "저는 이만 갈게요!", "안녕히 계세요!", "필요하면 트레이에 있을게요.", "펑! 사라졌어요."],
  },

  hi: {
    samples: [
      "नमस्ते! जब मैं बोल रही हूँ तब मुझ पर क्लिक करें रुकने के लिए, और फिर से क्लिक करें जारी रखने के लिए।",
      "स्क्रीन पर कोई भी टेक्स्ट चुनें और अपना शॉर्टकट दबाएँ — मैं उसे ज़ोर से पढ़ दूँगी।",
      "मुझ पर राइट-क्लिक करें और मेरी छोटी जादू की किताब खोलें, जहाँ आप कुछ लिखवा सकते हैं।",
      "कड़ाही उबल रही है और चाँद ऊँचा है — आपसे बातें करने के लिए मेरे पास पूरी रात है।",
      "मैं पच्चीस अलग-अलग आवाज़ों में बोल सकती हूँ। चलिए, एक नई आवाज़ चुनिए।",
      "ध्यान रखिए कि आपने क्लिपबोर्ड में क्या छोड़ा है… मैं उसे ज़ोर से पढ़ सकती हूँ।",
      "हर अच्छी जादूगरनी को एक साथी चाहिए। आज वह आप हैं। नमस्ते, मेरे साथी!",
      "वर्तनी जाँच? प्रिय, शब्दों का जादू तो मैं ही हूँ।",
    ],
    greetings: [
      "नमस्ते, मैं {name} हूँ!",
      "{name}, आपकी सेवा में।",
      "आपने {name} को चुना। मिलकर खुशी हुई!",
      "यह {name} है। हम क्या कहें?",
      "नमस्ते, मैं {name} बोल रही हूँ।",
    ],
    hellos: ["मैं वापस आ गई!", "क्या आपने मुझे याद किया?", "देखो, मैं यहाँ हूँ!", "लो जी!", "जादू ड्यूटी के लिए तैयार।"],
    goodbyes: ["जल्द मिलते हैं!", "मैं चलती हूँ!", "अभी के लिए अलविदा!", "ज़रूरत हो तो मैं ट्रे में रहूँगी।", "पूफ़! गायब।"],
  },

  zh: {
    samples: [
      "你好！我说话的时候点我一下就能暂停，再点一下就继续。",
      "在屏幕上选中任意文字，然后按下你的快捷键，我就会念出来。",
      "右键点我，打开我的小魔法书，在里面输入想让我说的话。",
      "锅在咕嘟咕嘟地冒泡，月亮也高高挂起——我有一整晚陪你聊天。",
      "我能用二十五种不同的声音说话。来吧，换一个新的试试。",
      "小心你剪贴板里留下的东西……我可能会把它念出来哦。",
      "每个好女巫都需要一个伙伴。今天就是你啦。你好，我的小伙伴！",
      "拼写检查？亲爱的，文字的魔法就是我本人。",
    ],
    greetings: [
      "你好，我是{name}！",
      "{name}，随时为你效劳。",
      "你选了{name}。很高兴认识你！",
      "我是{name}，我们说点什么呢？",
      "嘿，我是{name}，准备好了就开始吧。",
    ],
    hellos: ["我回来啦！", "想我了吗？", "藏猫猫！", "噔噔！", "魔法待命，随时出发。"],
    goodbyes: ["回头见！", "我先溜啦！", "暂时再见！", "需要我就到托盘里找我。", "噗！不见了。"],
  },
};
