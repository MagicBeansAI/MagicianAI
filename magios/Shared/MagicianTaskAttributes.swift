import ActivityKit
import Foundation

public struct MagicianTaskAttributes: ActivityAttributes {
    public struct ContentState: Codable, Hashable {
        public var status: String
        public var isDone: Bool
        public var stepCount: Int
        public var cardTitle: String
        
        public init(status: String, isDone: Bool, stepCount: Int = 0, cardTitle: String = "") {
            self.status = status
            self.isDone = isDone
            self.stepCount = stepCount
            self.cardTitle = cardTitle
        }
    }

    public var taskName: String
    
    public init(taskName: String) {
        self.taskName = taskName
    }
}
