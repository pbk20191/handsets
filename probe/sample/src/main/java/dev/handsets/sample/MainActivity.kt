package dev.handsets.sample

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent { MaterialTheme { Screen() } }
    }
}

private val FRUITS = listOf("사과", "바나나", "체리", "Durian", "Elderberry", "무화과", "Grape", "Honeydew")

@androidx.compose.runtime.Composable
private fun Screen() {
    var query by remember { mutableStateOf("") }
    var onlyKorean by remember { mutableStateOf(false) }
    var searched by remember { mutableStateOf<String?>(null) }
    var confirmReset by remember { mutableStateOf(false) }
    val shown = FRUITS.filter { it.contains(query, ignoreCase = true) && (!onlyKorean || it.first() >= '가') }

    Column(Modifier.fillMaxSize().padding(16.dp).testTag("screen")) {
        Text("과일 검색", style = MaterialTheme.typography.headlineSmall, modifier = Modifier.testTag("title"))
        OutlinedTextField(
            value = query,
            onValueChange = { query = it },
            label = { Text("검색어") },
            modifier = Modifier.fillMaxWidth().testTag("query"),
        )
        Row(Modifier.padding(vertical = 8.dp)) {
            Text("한글만", Modifier.padding(end = 8.dp))
            Switch(checked = onlyKorean, onCheckedChange = { onlyKorean = it }, modifier = Modifier.testTag("korean_only"))
        }
        // The button's label merges into it in the accessibility tree; the
        // unmerged semantics tree keeps the Text as its own node.
        Row {
            Button(onClick = { searched = query }, modifier = Modifier.testTag("search")) {
                Text("검색 (${shown.size})")
            }
            TextButton(onClick = { confirmReset = true }, modifier = Modifier.testTag("reset")) {
                Text("초기화")
            }
        }
        // A dialog is its own window, outside the activity's view tree.
        if (confirmReset) {
            AlertDialog(
                onDismissRequest = { confirmReset = false },
                title = { Text("검색 초기화") },
                text = { Text("검색어와 필터를 지울까요?") },
                confirmButton = {
                    TextButton(onClick = {
                        query = ""; onlyKorean = false; searched = null; confirmReset = false
                    }, modifier = Modifier.testTag("reset_ok")) { Text("지우기") }
                },
                dismissButton = {
                    TextButton(onClick = { confirmReset = false }, modifier = Modifier.testTag("reset_cancel")) { Text("취소") }
                },
                modifier = Modifier.testTag("reset_dialog"),
            )
        }
        AnimatedVisibility(searched != null) {
            Text("'${searched}' 결과 ${shown.size}개", Modifier.testTag("result_banner"))
        }
        LazyColumn(Modifier.testTag("list")) {
            items(shown) { fruit ->
                Card(Modifier.fillMaxWidth().padding(vertical = 4.dp).testTag("item:$fruit")) {
                    Text(fruit, Modifier.padding(12.dp))
                }
            }
        }
    }
}
